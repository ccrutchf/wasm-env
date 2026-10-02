//! `double` in software: IEEE 754 binary64 on 32-bit integers, for GPUs
//! without f64 (most; WGSL has none), and the same everywhere. A double is
//! its bits as `vec2<u32>` (low word first), as a 64-bit integer is.
//! Rounding is to nearest, ties to even, with denormals; a NaN result is
//! the operand's (quieted) or the default one.
//!
//! The arithmetic is exact, then rounded once (`f64_round`): fma's product
//! and sum as 128-bit integers (`vec4<u32>`, low word first), addition in
//! 64 bits, multiplication's 106-bit product; division and square roots
//! bit by bit. Where the device's own doubles are OpenCL's (`NATIVE`; not
//! lavapipe's, which flush denormals), the arithmetic is those.

/// The library (name, what it calls, WGSL), as `wgsl::LIBRARY`.
pub(crate) const LIBRARY: &[(&str, &[&str], &str)] = &[
    ("u128_add", &[], "fn u128_add(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    let x = a.x + b.x;
    let c0 = select(0u, 1u, x < a.x);
    let y1 = a.y + b.y;
    let y = y1 + c0;
    let c1 = select(0u, 1u, y1 < a.y || y < y1);
    let z1 = a.z + b.z;
    let z = z1 + c1;
    let c2 = select(0u, 1u, z1 < a.z || z < z1);
    return vec4<u32>(x, y, z, a.w + b.w + c2);
}"),
    ("u128_sub", &[], "fn u128_sub(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    let b0 = select(0u, 1u, a.x < b.x);
    let y1 = a.y - b.y;
    let b1 = select(0u, 1u, a.y < b.y || y1 < b0);
    let z1 = a.z - b.z;
    let b2 = select(0u, 1u, a.z < b.z || z1 < b1);
    return vec4<u32>(a.x - b.x, y1 - b0, z1 - b1, a.w - b.w - b2);
}"),
    ("u128_lt", &[], "fn u128_lt(a: vec4<u32>, b: vec4<u32>) -> bool {
    if (a.w != b.w) { return a.w < b.w; }
    if (a.z != b.z) { return a.z < b.z; }
    if (a.y != b.y) { return a.y < b.y; }
    return a.x < b.x;
}"),
    // (128 for zero.)
    ("u128_clz", &[], "fn u128_clz(a: vec4<u32>) -> u32 {
    if (a.w != 0u) { return countLeadingZeros(a.w); }
    if (a.z != 0u) { return 32u + countLeadingZeros(a.z); }
    if (a.y != 0u) { return 64u + countLeadingZeros(a.y); }
    return 96u + countLeadingZeros(a.x);
}"),
    // (n under 128.)
    ("u128_shl", &[], "fn u128_shl(a: vec4<u32>, n: u32) -> vec4<u32> {
    var r = a;
    var k = n;
    if (k >= 64u) { r = vec4<u32>(0u, 0u, r.x, r.y); k -= 64u; }
    if (k >= 32u) { r = vec4<u32>(0u, r.x, r.y, r.z); k -= 32u; }
    if (k == 0u) { return r; }
    return vec4<u32>(r.x << k, (r.y << k) | (r.x >> (32u - k)), (r.z << k) | (r.y >> (32u - k)), (r.w << k) | (r.z >> (32u - k)));
}"),
    // a >> n, the bits shifted out OR'd into bit 0 (sticky), for any n.
    ("u128_shr_sticky", &[], "fn u128_shr_sticky(a: vec4<u32>, n: u32) -> vec4<u32> {
    if (n == 0u) { return a; }
    if (n >= 128u) { return vec4<u32>(select(0u, 1u, any(a != vec4<u32>(0u))), 0u, 0u, 0u); }
    var r = a;
    var k = n;
    var lost = 0u;
    if (k >= 64u) { lost = r.x | r.y; r = vec4<u32>(r.z, r.w, 0u, 0u); k -= 64u; }
    if (k >= 32u) { lost = lost | r.x; r = vec4<u32>(r.y, r.z, r.w, 0u); k -= 32u; }
    if (k > 0u) {
        lost = lost | (r.x << (32u - k));
        r = vec4<u32>((r.x >> k) | (r.y << (32u - k)), (r.y >> k) | (r.z << (32u - k)), (r.z >> k) | (r.w << (32u - k)), r.w >> k);
    }
    r.x = r.x | select(0u, 1u, lost != 0u);
    return r;
}"),
    ("f64_is_nan", &[], "fn f64_is_nan(a: vec2<u32>) -> bool {
    let h = a.y & 0x7fffffffu;
    return h > 0x7ff00000u || (h == 0x7ff00000u && a.x != 0u);
}"),
    ("f64_is_inf", &[], "fn f64_is_inf(a: vec2<u32>) -> bool { return (a.y & 0x7fffffffu) == 0x7ff00000u && a.x == 0u; }"),
    ("f64_is_zero", &[], "fn f64_is_zero(a: vec2<u32>) -> bool { return (a.y & 0x7fffffffu) == 0u && a.x == 0u; }"),
    ("f64_quiet", &[], "fn f64_quiet(a: vec2<u32>) -> vec2<u32> { return vec2<u32>(a.x, a.y | 0x00080000u); }"),
    // A finite double's significand (with its leading bit) and the
    // exponent of its last bit: the value is significand * 2^exponent.
    ("f64_mant", &[], "fn f64_mant(a: vec2<u32>) -> vec2<u32> {
    let e = (a.y >> 20u) & 0x7ffu;
    return vec2<u32>(a.x, (a.y & 0xfffffu) | select(0u, 0x100000u, e != 0u));
}"),
    ("f64_exp", &[], "fn f64_exp(a: vec2<u32>) -> i32 { return i32(max((a.y >> 20u) & 0x7ffu, 1u)) - 1075i; }"),
    // m * 2^e (m not 0) rounded to a double, with the sign bit given:
    // to 53 bits, or to the denormals' last bit, 2^-1074. The exponent
    // field is added to the significand, so a significand rounded up to
    // 2^53 carries into it (and the largest, into infinity's).
    ("f64_round", &["u128_clz", "u128_shl", "u128_shr_sticky", "add64"], "fn f64_round(sign: u32, m: vec4<u32>, e: i32) -> vec2<u32> {
    let top = 127i - i32(u128_clz(m));
    if (top + e + 1023i >= 2047i) { return vec2<u32>(0u, sign | 0x7ff00000u); }
    let d = max(top - 52i, -1074i - e);
    var q: vec2<u32>;
    if (d <= 0i) {
        q = u128_shl(m, u32(-d)).xy;
    } else {
        // (Its last two bits: the first bit dropped, and whether any other was.)
        var s: vec4<u32>;
        if (d >= 2i) { s = u128_shr_sticky(m, u32(d - 2i)); } else { s = u128_shl(m, 1u); }
        q = vec2<u32>((s.x >> 2u) | (s.y << 30u), (s.y >> 2u) | (s.z << 30u));
        let half = (s.x & 2u) != 0u;
        let more = (s.x & 1u) != 0u;
        if (half && (more || (q.x & 1u) != 0u)) { q = add64(q, vec2<u32>(1u, 0u)); }
    }
    let r = add64(vec2<u32>(0u, u32(e + d + 1074i) << 20u), q);
    return vec2<u32>(r.x, r.y | sign);
}"),
    // a * b + c, rounded once.
    ("f64_fma", &["f64_is_nan", "f64_is_inf", "f64_is_zero", "f64_quiet", "f64_mant", "f64_exp", "f64_round", "mul64", "umulhi64", "u128_clz", "u128_shl", "u128_shr_sticky", "u128_add", "u128_sub", "u128_lt"], "fn f64_fma(a: vec2<u32>, b: vec2<u32>, c: vec2<u32>) -> vec2<u32> {
    let nan = vec2<u32>(0u, 0x7ff80000u);
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    let sp = (a.y ^ b.y) & 0x80000000u;
    let pz = f64_is_zero(a) || f64_is_zero(b);
    let pinf = f64_is_inf(a) || f64_is_inf(b);
    if (pinf && pz) { return nan; }
    if (f64_is_nan(c)) { return f64_quiet(c); }
    let sc = c.y & 0x80000000u;
    if (pinf) {
        if (f64_is_inf(c) && sc != sp) { return nan; }
        return vec2<u32>(0u, sp | 0x7ff00000u);
    }
    if (f64_is_inf(c)) { return c; }
    if (pz) {
        // (Zeros: +0 unless both are -0.)
        if (f64_is_zero(c)) { return vec2<u32>(0u, sp & sc); }
        return c;
    }
    let ma = f64_mant(a);
    let mb = f64_mant(b);
    let p = vec4<u32>(mul64(ma, mb), umulhi64(ma, mb));
    let ep = f64_exp(a) + f64_exp(b);
    if (f64_is_zero(c)) { return f64_round(sp, p, ep); }
    let cm = vec4<u32>(f64_mant(c), 0u, 0u);
    let ec = f64_exp(c);
    // Summed in a frame where the one with the higher leading bit has it
    // at bit 125; the other's bits below the frame are sticky.
    var x = p;
    var ex = ep;
    var sx = sp;
    var y = cm;
    var ey = ec;
    var sy = sc;
    if (127i - i32(u128_clz(cm)) + ec > 127i - i32(u128_clz(p)) + ep) {
        x = cm; ex = ec; sx = sc;
        y = p; ey = ep; sy = sp;
    }
    let l = 125i - (127i - i32(u128_clz(x)));
    x = u128_shl(x, u32(l));
    let e0 = ex - l;
    if (ey < e0) { y = u128_shr_sticky(y, u32(e0 - ey)); } else if (ey > e0) { y = u128_shl(y, u32(ey - e0)); }
    var m: vec4<u32>;
    var s = sx;
    if (sx == sy) {
        m = u128_add(x, y);
    } else if (u128_lt(x, y)) {
        m = u128_sub(y, x);
        s = sy;
    } else {
        m = u128_sub(x, y);
    }
    if (all(m == vec4<u32>(0u))) { return vec2<u32>(0u, 0u); }
    return f64_round(s, m, e0);
}"),
    // a >> n, the bits shifted out OR'd into bit 0 (sticky), for any n.
    ("u64_shr_sticky", &["lshr64", "shl64"], "fn u64_shr_sticky(a: vec2<u32>, n: u32) -> vec2<u32> {
    if (n == 0u) { return a; }
    if (n >= 64u) { return vec2<u32>(select(0u, 1u, (a.x | a.y) != 0u), 0u); }
    let r = lshr64(a, n);
    let lost = a & ~shl64(vec2<u32>(0xffffffffu, 0xffffffffu), n);
    return vec2<u32>(r.x | select(0u, 1u, (lost.x | lost.y) != 0u), r.y);
}"),
    // Significands 10 bits up (63 bits at most, so the sum fits 64), the
    // smaller one aligned with its lost bits sticky.
    ("f64_add", &["f64_is_nan", "f64_is_inf", "f64_is_zero", "f64_quiet", "f64_mant", "f64_exp", "f64_round", "shl64", "u64_shr_sticky", "add64", "sub64", "ult64"], "fn f64_add(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    var sa = a.y & 0x80000000u;
    var sb = b.y & 0x80000000u;
    if (f64_is_inf(a)) {
        if (f64_is_inf(b) && sa != sb) { return vec2<u32>(0u, 0x7ff80000u); }
        return a;
    }
    if (f64_is_inf(b)) { return b; }
    if (f64_is_zero(a)) {
        if (f64_is_zero(b)) { return vec2<u32>(0u, sa & sb); }
        return b;
    }
    if (f64_is_zero(b)) { return a; }
    var ma = shl64(f64_mant(a), 10u);
    var ea = f64_exp(a) - 10i;
    var mb = shl64(f64_mant(b), 10u);
    var eb = f64_exp(b) - 10i;
    if (eb > ea) {
        let m = ma; ma = mb; mb = m;
        let e = ea; ea = eb; eb = e;
        let s = sa; sa = sb; sb = s;
    }
    mb = u64_shr_sticky(mb, u32(ea - eb));
    var m: vec2<u32>;
    var s = sa;
    if (sa == sb) {
        m = add64(ma, mb);
    } else if (ult64(ma, mb)) {
        m = sub64(mb, ma);
        s = sb;
    } else {
        m = sub64(ma, mb);
    }
    if ((m.x | m.y) == 0u) { return vec2<u32>(0u, 0u); }
    return f64_round(s, vec4<u32>(m, 0u, 0u), ea);
}"),
    ("f64_sub", &["f64_add"], "fn f64_sub(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return f64_add(a, vec2<u32>(b.x, b.y ^ 0x80000000u)); }"),
    // The 106-bit product, rounded.
    ("f64_mul", &["f64_is_nan", "f64_is_inf", "f64_is_zero", "f64_quiet", "f64_mant", "f64_exp", "f64_round", "mul64", "umulhi64"], "fn f64_mul(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    let s = (a.y ^ b.y) & 0x80000000u;
    let z = f64_is_zero(a) || f64_is_zero(b);
    let i = f64_is_inf(a) || f64_is_inf(b);
    if (z && i) { return vec2<u32>(0u, 0x7ff80000u); }
    if (i) { return vec2<u32>(0u, s | 0x7ff00000u); }
    if (z) { return vec2<u32>(0u, s); }
    let ma = f64_mant(a);
    let mb = f64_mant(b);
    return f64_round(s, vec4<u32>(mul64(ma, mb), umulhi64(ma, mb)), f64_exp(a) + f64_exp(b));
}"),
    // A finite, non-zero double's significand shifted so its leading bit
    // is bit 52, and the exponent of its last bit then.
    ("f64_normal", &["f64_mant", "f64_exp", "shl64"], "fn f64_normal(a: vec2<u32>) -> vec3<u32> {
    let m = f64_mant(a);
    let z = select(32u + countLeadingZeros(m.x), countLeadingZeros(m.y), m.y != 0u) - 11u;
    return vec3<u32>(shl64(m, z), bitcast<u32>(f64_exp(a) - i32(z)));
}"),
    // The quotient bit by bit: 64 bits of it (the significands' ratio is
    // under 2), the remainder sticky.
    ("f64_div", &["f64_is_nan", "f64_is_inf", "f64_is_zero", "f64_quiet", "f64_normal", "f64_round", "shl64", "sub64", "ult64"], "fn f64_div(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let nan = vec2<u32>(0u, 0x7ff80000u);
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_nan(b)) { return f64_quiet(b); }
    let s = (a.y ^ b.y) & 0x80000000u;
    if (f64_is_inf(a)) {
        if (f64_is_inf(b)) { return nan; }
        return vec2<u32>(0u, s | 0x7ff00000u);
    }
    if (f64_is_inf(b)) { return vec2<u32>(0u, s); }
    if (f64_is_zero(b)) {
        if (f64_is_zero(a)) { return nan; }
        return vec2<u32>(0u, s | 0x7ff00000u);
    }
    if (f64_is_zero(a)) { return vec2<u32>(0u, s); }
    let na = f64_normal(a);
    let nb = f64_normal(b);
    let d = nb.xy;
    var r = na.xy;
    var q = vec2<u32>(0u, 0u);
    for (var i = 0u; i < 64u; i++) {
        q = shl64(q, 1u);
        if (!ult64(r, d)) { r = sub64(r, d); q.x = q.x | 1u; }
        r = shl64(r, 1u);
    }
    let m = vec4<u32>(q.x | select(0u, 1u, (r.x | r.y) != 0u), q.y, 0u, 0u);
    return f64_round(s, m, bitcast<i32>(na.z) - bitcast<i32>(nb.z) - 63i);
}"),
    // The root bit by bit, of the significand shifted 66 bits up (60 bits
    // of root), the remainder sticky.
    ("f64_sqrt", &["f64_is_nan", "f64_is_zero", "f64_quiet", "f64_normal", "f64_round", "u128_shl", "shl64", "sub64", "ult64"], "fn f64_sqrt(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return f64_quiet(a); }
    if (f64_is_zero(a)) { return a; }
    if ((a.y & 0x80000000u) != 0u) { return vec2<u32>(0u, 0x7ff80000u); }
    if (a.y >= 0x7ff00000u) { return a; }
    let n = f64_normal(a);
    var m = n.xy;
    var e = bitcast<i32>(n.z);
    if ((e & 1i) != 0i) { m = shl64(m, 1u); e -= 1i; }
    let rad = u128_shl(vec4<u32>(m, 0u, 0u), 66u);
    var rem = vec2<u32>(0u, 0u);
    var s = vec2<u32>(0u, 0u);
    for (var i = 0u; i < 64u; i++) {
        let p = 126u - 2u * i;
        rem = shl64(rem, 2u);
        rem.x = rem.x | ((rad[p >> 5u] >> (p & 31u)) & 3u);
        let t = shl64(s, 2u) | vec2<u32>(1u, 0u);
        s = shl64(s, 1u);
        if (!ult64(rem, t)) { rem = sub64(rem, t); s.x = s.x | 1u; }
    }
    let r = vec4<u32>(s.x | select(0u, 1u, (rem.x | rem.y) != 0u), s.y, 0u, 0u);
    return f64_round(0u, r, (e - 66i) / 2i);
}"),
    ("f64_from_f32", &["f64_round"], "fn f64_from_f32(f: f32) -> vec2<u32> {
    let u = bitcast<u32>(f);
    let s = u & 0x80000000u;
    let e = (u >> 23u) & 0xffu;
    let m = u & 0x7fffffu;
    if (e == 0xffu) { return vec2<u32>(m << 29u, s | 0x7ff00000u | (m >> 3u)); }
    if (e == 0u && m == 0u) { return vec2<u32>(0u, s); }
    return f64_round(s, vec4<u32>(m | select(0u, 0x800000u, e != 0u), 0u, 0u, 0u), i32(max(e, 1u)) - 150i);
}"),
    // To a float: rounded the same way, to 24 bits or 2^-149 (dropping at
    // least 29 bits: a double has more).
    ("f64_to_f32", &["f64_is_nan", "f64_is_zero", "f64_mant", "f64_exp", "u128_shr_sticky"], "fn f64_to_f32(a: vec2<u32>) -> f32 {
    let s = a.y & 0x80000000u;
    if (f64_is_nan(a)) { return bitcast<f32>(s | 0x7fc00000u | ((a.y & 0x7ffffu) << 3u)); }
    if ((a.y & 0x7fffffffu) >= 0x7ff00000u) { return bitcast<f32>(s | 0x7f800000u); }
    if (f64_is_zero(a)) { return bitcast<f32>(s); }
    let m = f64_mant(a);
    let e = f64_exp(a);
    let top = select(31i - i32(countLeadingZeros(m.x)), 63i - i32(countLeadingZeros(m.y)), m.y != 0u);
    if (top + e + 127i >= 255i) { return bitcast<f32>(s | 0x7f800000u); }
    let d = max(top - 23i, -149i - e);
    let r = u128_shr_sticky(vec4<u32>(m, 0u, 0u), u32(d - 2i));
    var q = (r.x >> 2u) | (r.y << 30u);
    if ((r.x & 2u) != 0u && ((r.x & 1u) != 0u || (q & 1u) != 0u)) { q += 1u; }
    return bitcast<f32>(s | ((u32(e + d + 149i) << 23u) + q));
}"),
    ("f64_from_u64", &["f64_round"], "fn f64_from_u64(x: vec2<u32>) -> vec2<u32> {
    if ((x.x | x.y) == 0u) { return vec2<u32>(0u, 0u); }
    return f64_round(0u, vec4<u32>(x, 0u, 0u), 0i);
}"),
    ("f64_from_i64", &["f64_round", "neg64"], "fn f64_from_i64(x: vec2<u32>) -> vec2<u32> {
    if ((x.x | x.y) == 0u) { return vec2<u32>(0u, 0u); }
    let s = x.y & 0x80000000u;
    return f64_round(s, vec4<u32>(select(x, neg64(x), s != 0u), 0u, 0u), 0i);
}"),
    // Toward zero, saturating (NaN: 0).
    ("f64_to_u64", &["f64_is_nan", "f64_mant", "f64_exp", "shl64", "lshr64"], "fn f64_to_u64(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a) || (a.y & 0x80000000u) != 0u) { return vec2<u32>(0u, 0u); }
    let m = f64_mant(a);
    let e = f64_exp(a);
    if (a.y >= 0x7ff00000u || e > 11i) { return vec2<u32>(0xffffffffu, 0xffffffffu); }
    if (e >= 0i) { return shl64(m, u32(e)); }
    if (e <= -64i) { return vec2<u32>(0u, 0u); }
    return lshr64(m, u32(-e));
}"),
    ("f64_to_i64", &["f64_is_nan", "f64_to_u64", "neg64"], "fn f64_to_i64(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return vec2<u32>(0u, 0u); }
    let s = (a.y & 0x80000000u) != 0u;
    let m = f64_to_u64(vec2<u32>(a.x, a.y & 0x7fffffffu));
    if (s) {
        if (m.y >= 0x80000000u) { return vec2<u32>(0u, 0x80000000u); }
        return neg64(m);
    }
    if (m.y >= 0x80000000u) { return vec2<u32>(0xffffffffu, 0x7fffffffu); }
    return m;
}"),
    // Ordered comparisons (false with a NaN). Doubles order as their bits
    // do, made unsigned (negatives' complemented); zeros are equal.
    ("f64_lt", &["f64_is_nan", "f64_is_zero", "ult64"], "fn f64_lt(a: vec2<u32>, b: vec2<u32>) -> bool {
    if (f64_is_nan(a) || f64_is_nan(b) || (f64_is_zero(a) && f64_is_zero(b))) { return false; }
    let ka = select(vec2<u32>(a.x, a.y | 0x80000000u), ~a, (a.y >> 31u) != 0u);
    let kb = select(vec2<u32>(b.x, b.y | 0x80000000u), ~b, (b.y >> 31u) != 0u);
    return ult64(ka, kb);
}"),
    ("f64_eq", &["f64_is_nan", "f64_is_zero"], "fn f64_eq(a: vec2<u32>, b: vec2<u32>) -> bool {
    return !f64_is_nan(a) && !f64_is_nan(b) && (all(a == b) || (f64_is_zero(a) && f64_is_zero(b)));
}"),
    // Rounded to an integer: toward zero by masking the fraction's bits;
    // the others from it.
    ("f64_trunc", &["shl64"], "fn f64_trunc(a: vec2<u32>) -> vec2<u32> {
    let e = i32((a.y >> 20u) & 0x7ffu) - 1023i;
    if (e < 0i) { return vec2<u32>(0u, a.y & 0x80000000u); }
    if (e >= 52i) { return a; }
    return a & shl64(vec2<u32>(0xffffffffu, 0xffffffffu), u32(52i - e));
}"),
    ("f64_floor", &["f64_trunc", "f64_sub"], "fn f64_floor(a: vec2<u32>) -> vec2<u32> {
    let t = f64_trunc(a);
    if ((a.y >> 31u) != 0u && !all(t == a)) { return f64_sub(t, vec2<u32>(0u, 0x3ff00000u)); }
    return t;
}"),
    ("f64_ceil", &["f64_trunc", "f64_add"], "fn f64_ceil(a: vec2<u32>) -> vec2<u32> {
    let t = f64_trunc(a);
    if ((a.y >> 31u) == 0u && !all(t == a)) { return f64_add(t, vec2<u32>(0u, 0x3ff00000u)); }
    return t;
}"),
    // To nearest, ties to even: by adding and subtracting 2^52.
    ("f64_rint", &["f64_add", "f64_sub"], "fn f64_rint(a: vec2<u32>) -> vec2<u32> {
    if (((a.y >> 20u) & 0x7ffu) >= 1075u) { return a; }
    let big = vec2<u32>(0u, 0x43300000u | (a.y & 0x80000000u));
    let r = f64_sub(f64_add(a, big), big);
    return vec2<u32>(r.x, (r.y & 0x7fffffffu) | (a.y & 0x80000000u));
}"),
    // Ties away from zero.
    ("f64_round_half_away", &["f64_trunc", "f64_sub", "f64_add", "f64_lt"], "fn f64_round_half_away(a: vec2<u32>) -> vec2<u32> {
    let t = f64_trunc(a);
    let d = f64_sub(a, t);
    if (f64_lt(vec2<u32>(d.x, d.y & 0x7fffffffu), vec2<u32>(0u, 0x3fe00000u))) { return t; }
    return f64_add(t, vec2<u32>(0u, 0x3ff00000u | (a.y & 0x80000000u)));
}"),
    // fmin and fmax: a NaN gives the other.
    ("f64_min", &["f64_is_nan", "f64_lt"], "fn f64_min(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return b; }
    if (f64_is_nan(b)) { return a; }
    return select(a, b, f64_lt(b, a));
}"),
    ("f64_max", &["f64_is_nan", "f64_lt"], "fn f64_max(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return b; }
    if (f64_is_nan(b)) { return a; }
    return select(a, b, f64_lt(a, b));
}"),
    // The next float or double up or down (from a zero, the smallest
    // denormal; from an infinity, the largest finite toward it).
    ("f32_next", &[], "fn f32_next(r: f32, up: bool) -> f32 {
    let u = bitcast<u32>(r);
    if ((u & 0x7fffffffu) == 0u) { return bitcast<f32>(select(0x80000001u, 1u, up)); }
    return bitcast<f32>(select(u - 1u, u + 1u, ((u >> 31u) == 0u) == up));
}"),
    ("f64_next", &["add64", "sub64"], "fn f64_next(r: vec2<u32>, up: bool) -> vec2<u32> {
    if (((r.y & 0x7fffffffu) | r.x) == 0u) { return vec2<u32>(1u, select(0x80000000u, 0u, up)); }
    return select(sub64(r, vec2<u32>(1u, 0u)), add64(r, vec2<u32>(1u, 0u)), ((r.y >> 31u) == 0u) == up);
}"),
    // Rounded otherwise than to nearest (mode 1 toward zero, 2 up, 3 down):
    // to nearest, then a step where that went the wrong way.
    ("f64_to_f32_dir", &["f64_to_f32", "f64_from_f32", "f64_lt", "f64_is_nan", "f32_next"], "fn f64_to_f32_dir(a: vec2<u32>, mode: u32) -> f32 {
    let r = f64_to_f32(a);
    if (f64_is_nan(a)) { return r; }
    let d = f64_from_f32(r);
    let neg = (a.y >> 31u) != 0u;
    if ((mode == 2u || (mode == 1u && neg)) && f64_lt(d, a)) { return f32_next(r, true); }
    if ((mode == 3u || (mode == 1u && !neg)) && f64_lt(a, d)) { return f32_next(r, false); }
    return r;
}"),
    // An integer's double (x a long, or an unsigned long), likewise. (A
    // double from a long is an integer: compared as one, unless it's out
    // of the long's range.)
    ("f64_from_int_dir", &["f64_from_i64", "f64_from_u64", "f64_to_i64", "f64_to_u64", "f64_next", "slt64", "ult64"], "fn f64_from_int_dir(x: vec2<u32>, signed: bool, mode: u32) -> vec2<u32> {
    var r: vec2<u32>;
    var above: bool;
    var below: bool;
    if (signed) {
        r = f64_from_i64(x);
        let big = (r.y & 0x80000000u) == 0u && r.y >= 0x43e00000u;
        let t = f64_to_i64(r);
        above = big || slt64(x, t);
        below = !big && slt64(t, x);
    } else {
        r = f64_from_u64(x);
        let big = r.y >= 0x43f00000u;
        let t = f64_to_u64(r);
        above = big || ult64(x, t);
        below = !big && ult64(t, x);
    }
    let neg = signed && (x.y >> 31u) != 0u;
    if ((mode == 2u || (mode == 1u && neg)) && below) { return f64_next(r, true); }
    if ((mode == 3u || (mode == 1u && !neg)) && above) { return f64_next(r, false); }
    return r;
}"),
    // `half` (as f32 in registers): a float rounded to half precision, to
    // nearest, ties to even, with half's denormals, its bits (WGSL's
    // pack2x16float leaves its rounding to the device).
    ("f32_to_f16", &[], "fn f32_to_f16(f: f32) -> u32 {
    let u = bitcast<u32>(f);
    let s = (u >> 16u) & 0x8000u;
    let e = i32((u >> 23u) & 0xffu);
    let m = u & 0x7fffffu;
    if (e == 0xff) { return s | 0x7c00u | select(0u, 0x200u | (m >> 13u), m != 0u); }
    // The value is full * 2^(e - 150); a half's last bit is 2^(e - 137) for a
    // normal one, at least 2^-24: d bits to drop.
    let full = m | select(0u, 0x800000u, e != 0);
    let d = max(13i, 126i - select(e, 1i, e == 0));
    if (d >= 25i) { return s; }
    var q = full >> u32(d);
    let rest = full & ((1u << u32(d)) - 1u);
    let mid = 1u << u32(d - 1i);
    if (rest > mid || (rest == mid && (q & 1u) != 0u)) { q += 1u; }
    // (The exponent field added to the significand: a carry rounds up.)
    let field = u32(max(select(e, 1i, e == 0) - 127i + 15i - 1i, 0i));
    let r = select(q, (field << 10u) + q, d == 13i);
    return s | min(r, 0x7c00u);
}"),
    ("f16_round", &["f32_to_f16"], "fn f16_round(f: f32) -> f32 { return unpack2x16float(f32_to_f16(f)).x; }"),
    ("f16_round2", &["f16_round"], "fn f16_round2(v: vec2<f32>) -> vec2<f32> { return vec2<f32>(f16_round(v.x), f16_round(v.y)); }"),
    ("f16_round3", &["f16_round"], "fn f16_round3(v: vec3<f32>) -> vec3<f32> { return vec3<f32>(f16_round(v.x), f16_round(v.y), f16_round(v.z)); }"),
    ("f16_round4", &["f16_round"], "fn f16_round4(v: vec4<f32>) -> vec4<f32> { return vec4<f32>(f16_round(v.x), f16_round(v.y), f16_round(v.z), f16_round(v.w)); }"),
    // Rounded otherwise than to nearest (mode 1 toward zero, 2 up, 3 down),
    // as an exact half: to nearest, then a step where that went the wrong
    // way.
    ("f16_round_dir", &["f32_to_f16"], "fn f16_round_dir(f: f32, mode: u32) -> f32 {
    var h = f32_to_f16(f);
    let r = unpack2x16float(h).x;
    if ((h & 0x7c00u) == 0x7c00u && (h & 0x3ffu) != 0u) { return r; }
    let neg = (h & 0x8000u) != 0u;
    let up = (mode == 2u || (mode == 1u && neg)) && r < f;
    let down = (mode == 3u || (mode == 1u && !neg)) && r > f;
    if (up || down) {
        // (Toward +inf on a positive magnitude is a bigger one.)
        if ((h & 0x7fffu) == 0u) { h = select(0x8001u, 1u, up); }
        else if (up != neg) { h += 1u; } else { h -= 1u; }
    }
    return unpack2x16float(h).x;
}"),
];

/// Where the device has f64 (and 64-bit integers, to get at a double's
/// bits): the arithmetic natively, under the same names. The rest (NaN
/// tests by the bits, conversions to and from integers) stays `LIBRARY`'s.
pub(crate) const NATIVE: &[(&str, &[&str], &str)] = &[
    ("f64_of", &[], "fn f64_of(a: vec2<u32>) -> f64 { return bitcast<f64>(u64(a.x) | (u64(a.y) << 32u)); }"),
    ("f64_bits", &[], "fn f64_bits(x: f64) -> vec2<u32> {
    let u = bitcast<u64>(x);
    return vec2<u32>(u32(u & 0xfffffffflu), u32(u >> 32u));
}"),
    ("f64_add", &["f64_of", "f64_bits"], "fn f64_add(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return f64_bits(f64_of(a) + f64_of(b)); }"),
    ("f64_sub", &["f64_of", "f64_bits"], "fn f64_sub(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return f64_bits(f64_of(a) - f64_of(b)); }"),
    ("f64_mul", &["f64_of", "f64_bits"], "fn f64_mul(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return f64_bits(f64_of(a) * f64_of(b)); }"),
    ("f64_div", &["f64_of", "f64_bits"], "fn f64_div(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return f64_bits(f64_of(a) / f64_of(b)); }"),
    ("f64_fma", &["f64_of", "f64_bits"], "fn f64_fma(a: vec2<u32>, b: vec2<u32>, c: vec2<u32>) -> vec2<u32> { return f64_bits(fma(f64_of(a), f64_of(b), f64_of(c))); }"),
    ("f64_sqrt", &["f64_of", "f64_bits"], "fn f64_sqrt(a: vec2<u32>) -> vec2<u32> { return f64_bits(sqrt(f64_of(a))); }"),
    ("f64_trunc", &["f64_of", "f64_bits"], "fn f64_trunc(a: vec2<u32>) -> vec2<u32> { return f64_bits(trunc(f64_of(a))); }"),
    ("f64_floor", &["f64_of", "f64_bits"], "fn f64_floor(a: vec2<u32>) -> vec2<u32> { return f64_bits(floor(f64_of(a))); }"),
    ("f64_ceil", &["f64_of", "f64_bits"], "fn f64_ceil(a: vec2<u32>) -> vec2<u32> { return f64_bits(ceil(f64_of(a))); }"),
    // (WGSL's round is to nearest, ties to even.)
    ("f64_rint", &["f64_of", "f64_bits"], "fn f64_rint(a: vec2<u32>) -> vec2<u32> { return f64_bits(round(f64_of(a))); }"),
    // Ordered comparisons, NaNs by their bits (a driver may take x == x
    // to be true).
    ("f64_lt", &["f64_of", "f64_is_nan"], "fn f64_lt(a: vec2<u32>, b: vec2<u32>) -> bool { return !f64_is_nan(a) && !f64_is_nan(b) && f64_of(a) < f64_of(b); }"),
    ("f64_eq", &["f64_of", "f64_is_nan"], "fn f64_eq(a: vec2<u32>, b: vec2<u32>) -> bool { return !f64_is_nan(a) && !f64_is_nan(b) && f64_of(a) == f64_of(b); }"),
    ("f64_from_f32", &["f64_bits"], "fn f64_from_f32(f: f32) -> vec2<u32> { return f64_bits(f64(f)); }"),
    ("f64_to_f32", &["f64_of"], "fn f64_to_f32(a: vec2<u32>) -> f32 { return f32(f64_of(a)); }"),
];
