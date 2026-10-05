@group(0) @binding(0) var<storage, read> args: array<u32>;
@group(0) @binding(1) var<storage, read_write> buf0: array<u32>;
@group(0) @binding(2) var<storage, read_write> buf1: array<u32>;
var<private> priv0: array<u32, 36>;
var<private> priv1: array<u32, 36>;
fn ld_args(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return args[i]; }
    return (args[i] >> s) | (args[i + 1u] << (32u - s));
}
fn ld_buf0(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return buf0[i]; }
    return (buf0[i] >> s) | (buf0[i + 1u] << (32u - s));
}
fn st_buf0(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { buf0[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        buf0[i] = (buf0[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_buf1(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return buf1[i]; }
    return (buf1[i] >> s) | (buf1[i + 1u] << (32u - s));
}
fn st_buf1(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { buf1[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        buf1[i] = (buf1[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_priv0(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return priv0[i]; }
    return (priv0[i] >> s) | (priv0[i + 1u] << (32u - s));
}
fn st_priv0(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { priv0[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        priv0[i] = (priv0[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn ld_priv1(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return priv1[i]; }
    return (priv1[i] >> s) | (priv1[i + 1u] << (32u - s));
}
fn st_priv1(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { priv1[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        priv1[i] = (priv1[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
    }
}
fn mul_hi_u32(a: u32, b: u32) -> u32 {
    let al = a & 0xffffu;
    let ah = a >> 16u;
    let bl = b & 0xffffu;
    let bh = b >> 16u;
    let lh = al * bh;
    let hl = ah * bl;
    let mid = ((al * bl) >> 16u) + (lh & 0xffffu) + (hl & 0xffffu);
    return ah * bh + (lh >> 16u) + (hl >> 16u) + (mid >> 16u);
}
fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    return vec2<u32>(lo, a.y + b.y + select(0u, 1u, lo < a.x));
}
fn sub64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x - b.x, a.y - b.y - select(0u, 1u, a.x < b.x));
}
fn mul64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    return vec2<u32>(a.x * b.x, mul_hi_u32(a.x, b.x) + a.x * b.y + a.y * b.x);
}
fn shl64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(0u, a.x << (n - 32u)); }
    return vec2<u32>(a.x << n, (a.y << n) | (a.x >> (32u - n)));
}
fn lshr64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(a.y >> (n - 32u), 0u); }
    return vec2<u32>((a.x >> n) | (a.y << (32u - n)), a.y >> n);
}
fn ult64(a: vec2<u32>, b: vec2<u32>) -> bool { return a.y < b.y || (a.y == b.y && a.x < b.x); }
fn umulhi64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let p01 = vec2<u32>(a.x * b.y, mul_hi_u32(a.x, b.y));
    let p10 = vec2<u32>(a.y * b.x, mul_hi_u32(a.y, b.x));
    let p11 = vec2<u32>(a.y * b.y, mul_hi_u32(a.y, b.y));
    let m1 = add64(p01, vec2<u32>(mul_hi_u32(a.x, b.x), 0u));
    let m2 = add64(m1, p10);
    let carry = select(0u, 1u, ult64(m2, m1));
    return add64(p11, vec2<u32>(m2.y, carry));
}
fn u128_add(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    let x = a.x + b.x;
    let c0 = select(0u, 1u, x < a.x);
    let y1 = a.y + b.y;
    let y = y1 + c0;
    let c1 = select(0u, 1u, y1 < a.y || y < y1);
    let z1 = a.z + b.z;
    let z = z1 + c1;
    let c2 = select(0u, 1u, z1 < a.z || z < z1);
    return vec4<u32>(x, y, z, a.w + b.w + c2);
}
fn u128_sub(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    let b0 = select(0u, 1u, a.x < b.x);
    let y1 = a.y - b.y;
    let b1 = select(0u, 1u, a.y < b.y || y1 < b0);
    let z1 = a.z - b.z;
    let b2 = select(0u, 1u, a.z < b.z || z1 < b1);
    return vec4<u32>(a.x - b.x, y1 - b0, z1 - b1, a.w - b.w - b2);
}
fn u128_lt(a: vec4<u32>, b: vec4<u32>) -> bool {
    if (a.w != b.w) { return a.w < b.w; }
    if (a.z != b.z) { return a.z < b.z; }
    if (a.y != b.y) { return a.y < b.y; }
    return a.x < b.x;
}
fn u128_clz(a: vec4<u32>) -> u32 {
    if (a.w != 0u) { return countLeadingZeros(a.w); }
    if (a.z != 0u) { return 32u + countLeadingZeros(a.z); }
    if (a.y != 0u) { return 64u + countLeadingZeros(a.y); }
    return 96u + countLeadingZeros(a.x);
}
fn u128_shl(a: vec4<u32>, n: u32) -> vec4<u32> {
    var r = a;
    var k = n;
    if (k >= 64u) { r = vec4<u32>(0u, 0u, r.x, r.y); k -= 64u; }
    if (k >= 32u) { r = vec4<u32>(0u, r.x, r.y, r.z); k -= 32u; }
    if (k == 0u) { return r; }
    return vec4<u32>(r.x << k, (r.y << k) | (r.x >> (32u - k)), (r.z << k) | (r.y >> (32u - k)), (r.w << k) | (r.z >> (32u - k)));
}
fn u128_shr_sticky(a: vec4<u32>, n: u32) -> vec4<u32> {
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
}
fn f64_is_nan(a: vec2<u32>) -> bool {
    let h = a.y & 0x7fffffffu;
    return h > 0x7ff00000u || (h == 0x7ff00000u && a.x != 0u);
}
fn f64_is_inf(a: vec2<u32>) -> bool { return (a.y & 0x7fffffffu) == 0x7ff00000u && a.x == 0u; }
fn f64_is_zero(a: vec2<u32>) -> bool { return (a.y & 0x7fffffffu) == 0u && a.x == 0u; }
fn f64_quiet(a: vec2<u32>) -> vec2<u32> { return vec2<u32>(a.x, a.y | 0x00080000u); }
fn f64_mant(a: vec2<u32>) -> vec2<u32> {
    let e = (a.y >> 20u) & 0x7ffu;
    return vec2<u32>(a.x, (a.y & 0xfffffu) | select(0u, 0x100000u, e != 0u));
}
fn f64_exp(a: vec2<u32>) -> i32 { return i32(max((a.y >> 20u) & 0x7ffu, 1u)) - 1075i; }
fn f64_round(sign: u32, m: vec4<u32>, e: i32) -> vec2<u32> {
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
}
fn f64_fma(a: vec2<u32>, b: vec2<u32>, c: vec2<u32>) -> vec2<u32> {
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
}
fn u64_shr_sticky(a: vec2<u32>, n: u32) -> vec2<u32> {
    if (n == 0u) { return a; }
    if (n >= 64u) { return vec2<u32>(select(0u, 1u, (a.x | a.y) != 0u), 0u); }
    let r = lshr64(a, n);
    let lost = a & ~shl64(vec2<u32>(0xffffffffu, 0xffffffffu), n);
    return vec2<u32>(r.x | select(0u, 1u, (lost.x | lost.y) != 0u), r.y);
}
fn f64_add(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
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
}
fn f64_sub(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> { return f64_add(a, vec2<u32>(b.x, b.y ^ 0x80000000u)); }
fn f64_mul(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
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
}
fn f64_normal(a: vec2<u32>) -> vec3<u32> {
    let m = f64_mant(a);
    let z = select(32u + countLeadingZeros(m.x), countLeadingZeros(m.y), m.y != 0u) - 11u;
    return vec3<u32>(shl64(m, z), bitcast<u32>(f64_exp(a) - i32(z)));
}
fn f64_div(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
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
}
fn f64_sqrt(a: vec2<u32>) -> vec2<u32> {
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
}
fn f64_lt(a: vec2<u32>, b: vec2<u32>) -> bool {
    if (f64_is_nan(a) || f64_is_nan(b) || (f64_is_zero(a) && f64_is_zero(b))) { return false; }
    let ka = select(vec2<u32>(a.x, a.y | 0x80000000u), ~a, (a.y >> 31u) != 0u);
    let kb = select(vec2<u32>(b.x, b.y | 0x80000000u), ~b, (b.y >> 31u) != 0u);
    return ult64(ka, kb);
}
fn f64_eq(a: vec2<u32>, b: vec2<u32>) -> bool {
    return !f64_is_nan(a) && !f64_is_nan(b) && (all(a == b) || (f64_is_zero(a) && f64_is_zero(b)));
}

@compute @workgroup_size(1, 1, 1)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let grp = wid + vec3<u32>(args[7], args[8], args[9]);
    let gid = grp * vec3<u32>(1u, 1u, 1u) + lid + vec3<u32>(args[0], args[1], args[2]);
    var jump: u32 = 0u;
    let a0 = select(args[16] + 4096u, 0u, args[16] == 0xffffffffu);
    let a1 = select(args[17] + 4096u, 0u, args[17] == 0xffffffffu);
    var v0: u32;
    var v1: u32;
    var v2: u32;
    var v5: u32;
    var v6: u32;
    v0 = 4096u;
    v1 = 4096u;
    v2 = select(0u, gid[min(0u, 2u)], 0u < 3u);
    let v3 = a1 + v2 * 128u;
    let v4 = array<vec2<u32>, 16>(vec2<u32>(buf1[(((v3 + 0u)) - 4096u) >> 2u], buf1[((((v3 + 0u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 8u)) - 4096u) >> 2u], buf1[((((v3 + 8u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 16u)) - 4096u) >> 2u], buf1[((((v3 + 16u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 24u)) - 4096u) >> 2u], buf1[((((v3 + 24u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 32u)) - 4096u) >> 2u], buf1[((((v3 + 32u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 40u)) - 4096u) >> 2u], buf1[((((v3 + 40u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 48u)) - 4096u) >> 2u], buf1[((((v3 + 48u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 56u)) - 4096u) >> 2u], buf1[((((v3 + 56u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 64u)) - 4096u) >> 2u], buf1[((((v3 + 64u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 72u)) - 4096u) >> 2u], buf1[((((v3 + 72u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 80u)) - 4096u) >> 2u], buf1[((((v3 + 80u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 88u)) - 4096u) >> 2u], buf1[((((v3 + 88u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 96u)) - 4096u) >> 2u], buf1[((((v3 + 96u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 104u)) - 4096u) >> 2u], buf1[((((v3 + 104u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 112u)) - 4096u) >> 2u], buf1[((((v3 + 112u) + 4u)) - 4096u) >> 2u]), vec2<u32>(buf1[(((v3 + 120u)) - 4096u) >> 2u], buf1[((((v3 + 120u) + 4u)) - 4096u) >> 2u]));
    priv0[(((v0 + 0u)) - 4096u) >> 2u] = v4[0].x;
    priv0[((((v0 + 0u) + 4u)) - 4096u) >> 2u] = v4[0].y;
    priv0[(((v0 + 8u)) - 4096u) >> 2u] = v4[1].x;
    priv0[((((v0 + 8u) + 4u)) - 4096u) >> 2u] = v4[1].y;
    priv0[(((v0 + 16u)) - 4096u) >> 2u] = v4[2].x;
    priv0[((((v0 + 16u) + 4u)) - 4096u) >> 2u] = v4[2].y;
    priv0[(((v0 + 24u)) - 4096u) >> 2u] = v4[3].x;
    priv0[((((v0 + 24u) + 4u)) - 4096u) >> 2u] = v4[3].y;
    priv0[(((v0 + 32u)) - 4096u) >> 2u] = v4[4].x;
    priv0[((((v0 + 32u) + 4u)) - 4096u) >> 2u] = v4[4].y;
    priv0[(((v0 + 40u)) - 4096u) >> 2u] = v4[5].x;
    priv0[((((v0 + 40u) + 4u)) - 4096u) >> 2u] = v4[5].y;
    priv0[(((v0 + 48u)) - 4096u) >> 2u] = v4[6].x;
    priv0[((((v0 + 48u) + 4u)) - 4096u) >> 2u] = v4[6].y;
    priv0[(((v0 + 56u)) - 4096u) >> 2u] = v4[7].x;
    priv0[((((v0 + 56u) + 4u)) - 4096u) >> 2u] = v4[7].y;
    priv0[(((v0 + 64u)) - 4096u) >> 2u] = v4[8].x;
    priv0[((((v0 + 64u) + 4u)) - 4096u) >> 2u] = v4[8].y;
    priv0[(((v0 + 72u)) - 4096u) >> 2u] = v4[9].x;
    priv0[((((v0 + 72u) + 4u)) - 4096u) >> 2u] = v4[9].y;
    priv0[(((v0 + 80u)) - 4096u) >> 2u] = v4[10].x;
    priv0[((((v0 + 80u) + 4u)) - 4096u) >> 2u] = v4[10].y;
    priv0[(((v0 + 88u)) - 4096u) >> 2u] = v4[11].x;
    priv0[((((v0 + 88u) + 4u)) - 4096u) >> 2u] = v4[11].y;
    priv0[(((v0 + 96u)) - 4096u) >> 2u] = v4[12].x;
    priv0[((((v0 + 96u) + 4u)) - 4096u) >> 2u] = v4[12].y;
    priv0[(((v0 + 104u)) - 4096u) >> 2u] = v4[13].x;
    priv0[((((v0 + 104u) + 4u)) - 4096u) >> 2u] = v4[13].y;
    priv0[(((v0 + 112u)) - 4096u) >> 2u] = v4[14].x;
    priv0[((((v0 + 112u) + 4u)) - 4096u) >> 2u] = v4[14].y;
    priv0[(((v0 + 120u)) - 4096u) >> 2u] = v4[15].x;
    priv0[((((v0 + 120u) + 4u)) - 4096u) >> 2u] = v4[15].y;
    v6 = 0u;
    loop {
        loop {
            let v7 = v0 + v6 * 8u;
            let v8 = vec2<u32>(priv0[((v7) - 4096u) >> 2u], priv0[(((v7 + 4u)) - 4096u) >> 2u]);
            let v9 = vec2<u32>(v8.x, v8.y & 0x7fffffffu);
            let v10 = f64_lt(v8, vec2<u32>(0u, 0u));
            let v11 = v9;
            let v12 = lshr64(v11, vec2<u32>(52u, 0u).x);
            let v13 = v12.x;
            let v14 = v13 < 1022u;
            let v15 = f64_sub(vec2<u32>(0u, 1072693248u), v9);
            let v16 = f64_mul(v15, vec2<u32>(0u, 1071644672u));
            let v17 = f64_sqrt(v16);
            let v18 = f64_mul(v8, v8);
            let v19 = select(v16, v18, v14);
            let v20 = select(v17, v9, v14);
            let v21 = f64_fma(v19, vec2<u32>(1563562081u, 1057575270u), vec2<u32>(2275811637u, 1062331896u));
            let v22 = f64_fma(v19, v21, vec2<u32>(2428671632u, 3215730899u));
            let v23 = f64_fma(v19, v22, vec2<u32>(3250057049u, 1070703294u));
            let v24 = f64_fma(v19, v23, vec2<u32>(2116460204u, 3218897705u));
            let v25 = f64_fma(v19, v24, vec2<u32>(2147653684u, 1070407233u));
            let v26 = f64_fma(v19, vec2<u32>(696438390u, 1069226562u), vec2<u32>(3021535117u, 3220058698u));
            let v27 = f64_fma(v19, v26, vec2<u32>(1461570812u, 1074143265u));
            let v28 = f64_fma(v19, v27, vec2<u32>(4177776106u, 3221898822u));
            let v29 = f64_fma(v19, v28, vec2<u32>(536998440u, 1073075889u));
            let v30 = f64_div(v25, v29);
            let v31 = f64_mul(v19, v30);
            let v32 = f64_fma(v20, v31, vec2<u32>(856972295u, 3163661862u));
            let v33 = f64_add(v17, v32);
            let v34 = f64_div(v33, vec2<u32>(1413754136u, 1074340347u));
            let v35 = f64_fma(v34, vec2<u32>(0u, 3221225472u), vec2<u32>(0u, 1072693248u));
            let v36 = v17;
            let v37 = v36 & vec2<u32>(0u, 4294967295u);
            let v38 = v37;
            let v39 = vec2<u32>(v38.x, v38.y ^ 0x80000000u);
            let v40 = f64_fma(v39, v38, v19);
            let v41 = f64_add(v17, v38);
            let v42 = f64_div(v40, v41);
            let v43 = f64_mul(v20, vec2<u32>(0u, 1073741824u));
            let v44 = f64_mul(v43, v31);
            let v45 = f64_fma(v42, vec2<u32>(0u, 1073741824u), v44);
            let v46 = f64_fma(v38, vec2<u32>(0u, 1073741824u), v45);
            let v47 = f64_div(v46, vec2<u32>(1413754136u, 1074340347u));
            let v48 = select(v47, v35, v10);
            let v49 = f64_fma(v8, v31, v8);
            let v50 = f64_div(v49, vec2<u32>(1413754136u, 1074340347u));
            let v51 = f64_sub(vec2<u32>(0u, 1071644672u), v50);
            let v52 = select(v48, v51, v14);
            let v53 = f64_eq(v8, vec2<u32>(0u, 1072693248u));
            let v54 = select(vec2<u32>(0u, 2146959360u), vec2<u32>(0u, 0u), v53);
            let v55 = f64_eq(v8, vec2<u32>(0u, 3220176896u));
            let v56 = select(v54, vec2<u32>(0u, 1072693248u), v55);
            let v57 = v13 < 1023u;
            let v58 = select(v56, v52, v57);
            let v59 = v13 > 966u;
            let v60 = select(vec2<u32>(0u, 1071644672u), v58, v59);
            let v61 = v1 + v6 * 8u;
            priv1[((v61) - 4096u) >> 2u] = v60.x;
            priv1[(((v61 + 4u)) - 4096u) >> 2u] = v60.y;
            v5 = v6 + 1u;
            let v62 = v6 < 15u;
            if (v62) {
                v6 = v5;
            } else {
                break;
            }
        }
        if (jump != 0u) {
            if (jump == 3u) { jump = 0u; break; }
        }
        break;
    }
    let v63 = array<vec2<u32>, 16>(vec2<u32>(priv1[(((v1 + 0u)) - 4096u) >> 2u], priv1[((((v1 + 0u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 8u)) - 4096u) >> 2u], priv1[((((v1 + 8u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 16u)) - 4096u) >> 2u], priv1[((((v1 + 16u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 24u)) - 4096u) >> 2u], priv1[((((v1 + 24u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 32u)) - 4096u) >> 2u], priv1[((((v1 + 32u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 40u)) - 4096u) >> 2u], priv1[((((v1 + 40u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 48u)) - 4096u) >> 2u], priv1[((((v1 + 48u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 56u)) - 4096u) >> 2u], priv1[((((v1 + 56u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 64u)) - 4096u) >> 2u], priv1[((((v1 + 64u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 72u)) - 4096u) >> 2u], priv1[((((v1 + 72u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 80u)) - 4096u) >> 2u], priv1[((((v1 + 80u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 88u)) - 4096u) >> 2u], priv1[((((v1 + 88u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 96u)) - 4096u) >> 2u], priv1[((((v1 + 96u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 104u)) - 4096u) >> 2u], priv1[((((v1 + 104u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 112u)) - 4096u) >> 2u], priv1[((((v1 + 112u) + 4u)) - 4096u) >> 2u]), vec2<u32>(priv1[(((v1 + 120u)) - 4096u) >> 2u], priv1[((((v1 + 120u) + 4u)) - 4096u) >> 2u]));
    let v64 = a0 + v2 * 128u;
    buf0[(((v64 + 0u)) - 4096u) >> 2u] = v63[0].x;
    buf0[((((v64 + 0u) + 4u)) - 4096u) >> 2u] = v63[0].y;
    buf0[(((v64 + 8u)) - 4096u) >> 2u] = v63[1].x;
    buf0[((((v64 + 8u) + 4u)) - 4096u) >> 2u] = v63[1].y;
    buf0[(((v64 + 16u)) - 4096u) >> 2u] = v63[2].x;
    buf0[((((v64 + 16u) + 4u)) - 4096u) >> 2u] = v63[2].y;
    buf0[(((v64 + 24u)) - 4096u) >> 2u] = v63[3].x;
    buf0[((((v64 + 24u) + 4u)) - 4096u) >> 2u] = v63[3].y;
    buf0[(((v64 + 32u)) - 4096u) >> 2u] = v63[4].x;
    buf0[((((v64 + 32u) + 4u)) - 4096u) >> 2u] = v63[4].y;
    buf0[(((v64 + 40u)) - 4096u) >> 2u] = v63[5].x;
    buf0[((((v64 + 40u) + 4u)) - 4096u) >> 2u] = v63[5].y;
    buf0[(((v64 + 48u)) - 4096u) >> 2u] = v63[6].x;
    buf0[((((v64 + 48u) + 4u)) - 4096u) >> 2u] = v63[6].y;
    buf0[(((v64 + 56u)) - 4096u) >> 2u] = v63[7].x;
    buf0[((((v64 + 56u) + 4u)) - 4096u) >> 2u] = v63[7].y;
    buf0[(((v64 + 64u)) - 4096u) >> 2u] = v63[8].x;
    buf0[((((v64 + 64u) + 4u)) - 4096u) >> 2u] = v63[8].y;
    buf0[(((v64 + 72u)) - 4096u) >> 2u] = v63[9].x;
    buf0[((((v64 + 72u) + 4u)) - 4096u) >> 2u] = v63[9].y;
    buf0[(((v64 + 80u)) - 4096u) >> 2u] = v63[10].x;
    buf0[((((v64 + 80u) + 4u)) - 4096u) >> 2u] = v63[10].y;
    buf0[(((v64 + 88u)) - 4096u) >> 2u] = v63[11].x;
    buf0[((((v64 + 88u) + 4u)) - 4096u) >> 2u] = v63[11].y;
    buf0[(((v64 + 96u)) - 4096u) >> 2u] = v63[12].x;
    buf0[((((v64 + 96u) + 4u)) - 4096u) >> 2u] = v63[12].y;
    buf0[(((v64 + 104u)) - 4096u) >> 2u] = v63[13].x;
    buf0[((((v64 + 104u) + 4u)) - 4096u) >> 2u] = v63[13].y;
    buf0[(((v64 + 112u)) - 4096u) >> 2u] = v63[14].x;
    buf0[((((v64 + 112u) + 4u)) - 4096u) >> 2u] = v63[14].y;
    buf0[(((v64 + 120u)) - 4096u) >> 2u] = v63[15].x;
    buf0[((((v64 + 120u) + 4u)) - 4096u) >> 2u] = v63[15].y;
    return;
}


