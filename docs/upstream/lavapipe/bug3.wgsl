@group(0) @binding(0) var<storage, read> args: array<u32>;
@group(0) @binding(1) var<storage, read_write> buf0: array<u32>;
@group(0) @binding(2) var<storage, read_write> buf1: array<u32>;
var<private> glob0: array<u32, 40> = array<u32, 40>(3239834080u, 1948342284u, 2126806581u, 1269411821u, 3890096458u, 3320640540u, 3944722327u, 2830488734u, 4254759295u, 652033304u, 4215054124u, 3028137177u, 1177380905u, 1614789820u, 1610577942u, 2699853794u, 2117007332u, 3889328657u, 1491471692u, 4177847879u, 3231371645u, 303277681u, 3621042925u, 2633736712u, 2893202598u, 1215559799u, 3148359494u, 1266686244u, 3102279991u, 361159825u, 2393864943u, 1091041605u, 1449455886u, 1996713354u, 1598543059u, 1420292253u, 230242576u, 166886u, 0u, 0u);
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
fn ld_glob0(p: u32) -> u32 {
    let i = (p - 4096u) >> 2u;
    let s = (p & 3u) * 8u;
    if (s == 0u) { return glob0[i]; }
    return (glob0[i] >> s) | (glob0[i + 1u] << (32u - s));
}
fn st_glob0(p: u32, v: u32, bytes: u32) {
    if ((p & 3u) == 0u && bytes == 4u) { glob0[(p - 4096u) >> 2u] = v; return; }
    for (var k = 0u; k < bytes; k++) {
        let q = p + k;
        let i = (q - 4096u) >> 2u;
        let s = (q & 3u) * 8u;
        glob0[i] = (glob0[i] & ~(0xffu << s)) | (((v >> (8u * k)) & 0xffu) << s);
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
fn neg64(a: vec2<u32>) -> vec2<u32> { return sub64(vec2<u32>(0u, 0u), a); }
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
fn ashr64(a: vec2<u32>, s: u32) -> vec2<u32> {
    let n = s & 63u;
    let h = bitcast<i32>(a.y);
    if (n == 0u) { return a; }
    if (n >= 32u) { return vec2<u32>(bitcast<u32>(h >> (n - 32u)), bitcast<u32>(h >> 31u)); }
    return vec2<u32>((a.x >> n) | (a.y << (32u - n)), bitcast<u32>(h >> n));
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
fn f64_to_f32(a: vec2<u32>) -> f32 {
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
}
fn f64_from_u64(x: vec2<u32>) -> vec2<u32> {
    if ((x.x | x.y) == 0u) { return vec2<u32>(0u, 0u); }
    return f64_round(0u, vec4<u32>(x, 0u, 0u), 0i);
}
fn f64_to_u64(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a) || (a.y & 0x80000000u) != 0u) { return vec2<u32>(0u, 0u); }
    let m = f64_mant(a);
    let e = f64_exp(a);
    if (a.y >= 0x7ff00000u || e > 11i) { return vec2<u32>(0xffffffffu, 0xffffffffu); }
    if (e >= 0i) { return shl64(m, u32(e)); }
    if (e <= -64i) { return vec2<u32>(0u, 0u); }
    return lshr64(m, u32(-e));
}
fn f64_to_i64(a: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return vec2<u32>(0u, 0u); }
    let s = (a.y & 0x80000000u) != 0u;
    let m = f64_to_u64(vec2<u32>(a.x, a.y & 0x7fffffffu));
    if (s) {
        if (m.y >= 0x80000000u) { return vec2<u32>(0u, 0x80000000u); }
        return neg64(m);
    }
    if (m.y >= 0x80000000u) { return vec2<u32>(0xffffffffu, 0x7fffffffu); }
    return m;
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
fn f64_trunc(a: vec2<u32>) -> vec2<u32> {
    let e = i32((a.y >> 20u) & 0x7ffu) - 1023i;
    if (e < 0i) { return vec2<u32>(0u, a.y & 0x80000000u); }
    if (e >= 52i) { return a; }
    return a & shl64(vec2<u32>(0xffffffffu, 0xffffffffu), u32(52i - e));
}
fn f64_floor(a: vec2<u32>) -> vec2<u32> {
    let t = f64_trunc(a);
    if ((a.y >> 31u) != 0u && !all(t == a)) { return f64_sub(t, vec2<u32>(0u, 0x3ff00000u)); }
    return t;
}
fn f64_min(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    if (f64_is_nan(a)) { return b; }
    if (f64_is_nan(b)) { return a; }
    return select(a, b, f64_lt(b, a));
}

@compute @workgroup_size(4, 1, 1)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let grp = wid + vec3<u32>(args[7], args[8], args[9]);
    let gid = grp * vec3<u32>(4u, 1u, 1u) + lid + vec3<u32>(args[0], args[1], args[2]);
    var jump: u32 = 0u;
    let a0 = select(args[16] + 4096u, 0u, args[16] == 0xffffffffu);
    let a1 = select(args[17] + 4096u, 0u, args[17] == 0xffffffffu);
    var v0: u32;
    var v182: array<vec2<u32>, 2>;
    var v183: array<vec2<u32>, 2>;
    var v186: array<vec2<u32>, 2>;
    var v189: array<vec2<u32>, 2>;
    var v190: array<vec2<u32>, 2>;
    var v191: array<vec2<u32>, 2>;
    var v194: array<vec2<u32>, 2>;
    var v496: array<vec2<u32>, 2>;
    var v497: array<vec2<u32>, 2>;
    var v500: array<vec2<u32>, 2>;
    var v503: array<vec2<u32>, 2>;
    var v504: array<vec2<u32>, 2>;
    var v505: array<vec2<u32>, 2>;
    var v508: array<vec2<u32>, 2>;
    var v810: array<vec2<u32>, 2>;
    var v811: array<vec2<u32>, 2>;
    var v814: array<vec2<u32>, 2>;
    var v817: array<vec2<u32>, 2>;
    var v818: array<vec2<u32>, 2>;
    var v819: array<vec2<u32>, 2>;
    var v822: array<vec2<u32>, 2>;
    v0 = select(0u, gid[min(0u, 2u)], 0u < 3u);
    let v1 = v0 + 1u;
    let v2 = select(1u, args[3u + min(0u, 2u)], 0u < 3u);
    let v3 = v1 < v2;
    if (v3) {
        let v4 = v0 * 3u;
        let v5 = a0 + v4 * 8u;
        let v6 = vec2<u32>(buf0[((v5) - 4096u) >> 2u], buf0[(((v5 + 4u)) - 4096u) >> 2u]);
        let v7 = vec2<u32>(v6.x, v6.y & 0x7fffffffu);
        let v8 = !f64_lt(v7, vec2<u32>(0u, 1121976320u));
        let v9 = f64_fma(v7, vec2<u32>(1841940611u, 1071931184u), vec2<u32>(0u, 1071644672u));
        let v10 = f64_trunc(v9);
        let v11 = f64_mul(v10, vec2<u32>(1413754136u, 3220775419u));
        let v12 = vec2<u32>(v11.x, v11.y ^ 0x80000000u);
        let v13 = f64_fma(v10, vec2<u32>(1413754136u, 3220775419u), v12);
        let v14 = f64_mul(v10, vec2<u32>(856972294u, 3163661862u));
        let v15 = vec2<u32>(v14.x, v14.y ^ 0x80000000u);
        let v16 = f64_fma(v10, vec2<u32>(856972294u, 3163661862u), v15);
        let v17 = f64_mul(v10, vec2<u32>(688016904u, 3108773073u));
        let v18 = vec2<u32>(v17.x, v17.y ^ 0x80000000u);
        let v19 = f64_fma(v10, vec2<u32>(688016904u, 3108773073u), v18);
        let v20 = f64_add(v14, v13);
        let v21 = f64_sub(v20, v13);
        let v22 = f64_sub(v14, v21);
        let v23 = f64_add(v17, v22);
        let v24 = f64_add(v16, v23);
        let v25 = f64_add(v19, v24);
        let v26 = f64_add(v11, v20);
        let v27 = f64_sub(v26, v11);
        let v28 = f64_sub(v20, v27);
        let v29 = f64_add(v28, v25);
        let v30 = f64_sub(v29, v28);
        let v31 = f64_sub(v25, v30);
        let v32 = f64_add(v7, v26);
        let v33 = f64_add(v32, v29);
        let v34 = f64_sub(v33, v32);
        let v35 = f64_sub(v29, v34);
        let v36 = f64_add(v31, v35);
        let v37 = f64_to_i64(v10);
        let v38 = v37.x;
        let v39 = v7;
        let v40 = lshr64(v39, vec2<u32>(52u, 0u).x);
        let v41 = v40.x;
        let v42 = v41 + 4294966273u;
        let v43 = u32(bitcast<i32>(v42) >> 3u);
        let v44 = u32(max(bitcast<i32>(v43), bitcast<i32>(6u)));
        let v45 = 133u - v44;
        let v46 = v45 & 240u;
        let v47 = 4096u + v46;
        let v48 = vec2<u32>(glob0[((v47) - 4096u) >> 2u], glob0[(((v47 + 4u)) - 4096u) >> 2u]);
        let v49 = v47 + 8u;
        let v50 = vec2<u32>(glob0[((v49) - 4096u) >> 2u], glob0[(((v49 + 4u)) - 4096u) >> 2u]);
        let v51 = v47 + 16u;
        let v52 = vec2<u32>(glob0[((v51) - 4096u) >> 2u], glob0[(((v51 + 4u)) - 4096u) >> 2u]);
        let v53 = v47 + 24u;
        let v54 = vec2<u32>(glob0[((v53) - 4096u) >> 2u], glob0[(((v53 + 4u)) - 4096u) >> 2u]);
        let v55 = v47 + 32u;
        let v56 = vec2<u32>(glob0[((v55) - 4096u) >> 2u], glob0[(((v55 + 4u)) - 4096u) >> 2u]);
        let v57 = v48.x;
        let v58 = lshr64(v48, vec2<u32>(32u, 0u).x);
        let v59 = v58.x;
        let v60 = v50.x;
        let v61 = lshr64(v50, vec2<u32>(32u, 0u).x);
        let v62 = v61.x;
        let v63 = v52.x;
        let v64 = lshr64(v52, vec2<u32>(32u, 0u).x);
        let v65 = v64.x;
        let v66 = v54.x;
        let v67 = lshr64(v54, vec2<u32>(32u, 0u).x);
        let v68 = v67.x;
        let v69 = v56.x;
        let v70 = lshr64(v56, vec2<u32>(32u, 0u).x);
        let v71 = v70.x;
        let v72 = v45 >> 2u;
        let v73 = v72 & 3u;
        let v74 = v73 == 1u;
        let v75 = v73 == 2u;
        let v76 = v73 == 3u;
        let v77 = select(v57, v59, v74);
        let v78 = select(v77, v60, v75);
        let v79 = select(v78, v62, v76);
        let v80 = select(v59, v60, v74);
        let v81 = select(v80, v62, v75);
        let v82 = select(v81, v63, v76);
        let v83 = select(v60, v62, v74);
        let v84 = select(v83, v63, v75);
        let v85 = select(v84, v65, v76);
        let v86 = select(v62, v63, v74);
        let v87 = select(v86, v65, v75);
        let v88 = select(v87, v66, v76);
        let v89 = select(v63, v65, v74);
        let v90 = select(v89, v66, v75);
        let v91 = select(v90, v68, v76);
        let v92 = select(v65, v66, v74);
        let v93 = select(v92, v68, v75);
        let v94 = select(v93, v69, v76);
        let v95 = select(v66, v68, v74);
        let v96 = select(v95, v69, v75);
        let v97 = select(v96, v71, v76);
        let v98 = vec2<u32>(v82, 0u);
        let v99 = shl64(v98, vec2<u32>(32u, 0u).x);
        let v100 = vec2<u32>(v79, 0u);
        let v101 = v99 | v100;
        let v102 = v45 << 3u;
        let v103 = v102 & 24u;
        let v104 = vec2<u32>(v103, 0u);
        let v105 = ashr64(v101, v104.x);
        let v106 = vec2<u32>(v85, 0u);
        let v107 = shl64(v106, vec2<u32>(32u, 0u).x);
        let v108 = v107 | v98;
        let v109 = ashr64(v108, v104.x);
        let v110 = v109.x;
        let v111 = vec2<u32>(v88, 0u);
        let v112 = shl64(v111, vec2<u32>(32u, 0u).x);
        let v113 = v112 | v106;
        let v114 = ashr64(v113, v104.x);
        let v115 = v114.x;
        let v116 = vec2<u32>(v91, 0u);
        let v117 = shl64(v116, vec2<u32>(32u, 0u).x);
        let v118 = v117 | v111;
        let v119 = ashr64(v118, v104.x);
        let v120 = vec2<u32>(v94, 0u);
        let v121 = shl64(v120, vec2<u32>(32u, 0u).x);
        let v122 = v121 | v116;
        let v123 = ashr64(v122, v104.x);
        let v124 = v123.x;
        let v125 = vec2<u32>(v97, 0u);
        let v126 = shl64(v125, vec2<u32>(32u, 0u).x);
        let v127 = v126 | v120;
        let v128 = ashr64(v127, v104.x);
        let v129 = v128.x;
        let v130 = bitcast<i32>(v43) < bitcast<i32>(111u);
        let v131 = select(vec2<u32>(0u, 930086912u), vec2<u32>(0u, 1072693248u), v130);
        let v132 = f64_mul(v7, v131);
        let v133 = select(142606336u, 0u, v130);
        let v134 = v44 << 23u;
        let v135 = v133 - v134;
        let v136 = v135 + 982515712u;
        let v137 = vec2<u32>(v136, 0u);
        let v138 = shl64(v137, vec2<u32>(32u, 0u).x);
        let v139 = v138;
        let v140 = v105 & vec2<u32>(4294967295u, 0u);
        let v141 = v110 & 65535u;
        let v142 = v141 | v136;
        let v143 = vec2<u32>(v142, 0u);
        let v144 = shl64(v143, vec2<u32>(32u, 0u).x);
        let v145 = v144 | v140;
        let v146 = v145;
        let v147 = f64_sub(v146, v139);
        let v148 = v135 + 1032847360u;
        let v149 = vec2<u32>(v148, 0u);
        let v150 = shl64(v149, vec2<u32>(32u, 0u).x);
        let v151 = v150;
        let v152 = select(((v115 << (16u % 32u)) | (v110 >> (32u - (16u % 32u)))) & 4294967295u, v115, (16u % 32u) == 0u);
        let v153 = vec2<u32>(v152, 0u);
        let v154 = v115 >> 16u;
        let v155 = v154 | v148;
        let v156 = vec2<u32>(v155, 0u);
        let v157 = shl64(v156, vec2<u32>(32u, 0u).x);
        let v158 = v157 | v153;
        let v159 = v158;
        let v160 = f64_sub(v159, v151);
        let v161 = v135 + 1083179008u;
        let v162 = vec2<u32>(v161, 0u);
        let v163 = v119 & vec2<u32>(4294967295u, 0u);
        let v164 = v124 & 65535u;
        let v165 = v164 | v161;
        let v166 = vec2<u32>(v165, 0u);
        let v167 = v135 + 1133510656u;
        let v168 = vec2<u32>(v167, 0u);
        let v169 = select(((v129 << (16u % 32u)) | (v124 >> (32u - (16u % 32u)))) & 4294967295u, v129, (16u % 32u) == 0u);
        let v170 = vec2<u32>(v169, 0u);
        let v171 = v129 >> 16u;
        let v172 = v171 | v167;
        let v173 = vec2<u32>(v172, 0u);
        let v174 = f64_mul(v132, v147);
        let v175 = vec2<u32>(v174.x, v174.y ^ 0x80000000u);
        let v176 = f64_fma(v147, v132, v175);
        let v177 = f64_mul(v132, v160);
        let v178 = vec2<u32>(v177.x, v177.y ^ 0x80000000u);
        let v179 = f64_fma(v160, v132, v178);
        let v180 = array<vec2<u32>, 2>(v162, array<vec2<u32>, 2>()[1]);
        let v181 = array<vec2<u32>, 2>(v180[0], v168);
        v182[0] = shl64(v181[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
        v182[1] = shl64(v181[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
        v183[0] = v182[0];
        v183[1] = v182[1];
        let v184 = array<vec2<u32>, 2>(v166, array<vec2<u32>, 2>()[1]);
        let v185 = array<vec2<u32>, 2>(v184[0], v173);
        v186[0] = shl64(v185[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
        v186[1] = shl64(v185[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
        let v187 = array<vec2<u32>, 2>(v163, array<vec2<u32>, 2>()[1]);
        let v188 = array<vec2<u32>, 2>(v187[0], v170);
        v189[0] = v186[0] | v188[0];
        v189[1] = v186[1] | v188[1];
        v190[0] = v189[0];
        v190[1] = v189[1];
        v191[0] = f64_sub(v190[0], v183[0]);
        v191[1] = f64_sub(v190[1], v183[1]);
        let v192 = array<vec2<u32>, 2>(v132, array<vec2<u32>, 2>()[1]);
        let v193 = array<vec2<u32>, 2>(v192[0], v192[0]);
        v194[0] = f64_mul(v193[0], v191[0]);
        v194[1] = f64_mul(v193[1], v191[1]);
        let v195 = v194[0];
        let v196 = vec2<u32>(v195.x, v195.y ^ 0x80000000u);
        let v197 = v191[0];
        let v198 = f64_fma(v197, v132, v196);
        let v199 = v194[1];
        let v200 = vec2<u32>(v199.x, v199.y ^ 0x80000000u);
        let v201 = v191[1];
        let v202 = f64_fma(v201, v132, v200);
        let v203 = f64_add(v195, v199);
        let v204 = f64_sub(v203, v199);
        let v205 = f64_sub(v195, v204);
        let v206 = f64_add(v202, v205);
        let v207 = f64_sub(v206, v202);
        let v208 = f64_sub(v205, v207);
        let v209 = f64_add(v177, v206);
        let v210 = f64_sub(v209, v206);
        let v211 = f64_sub(v177, v210);
        let v212 = f64_add(v208, v211);
        let v213 = f64_add(v198, v212);
        let v214 = f64_sub(v213, v198);
        let v215 = f64_sub(v212, v214);
        let v216 = f64_add(v174, v213);
        let v217 = f64_sub(v216, v213);
        let v218 = f64_sub(v174, v217);
        let v219 = f64_add(v215, v218);
        let v220 = f64_add(v179, v219);
        let v221 = f64_add(v176, v220);
        let v222 = f64_mul(v203, vec2<u32>(0u, 1062207488u));
        let v223 = f64_floor(v222);
        let v224 = f64_sub(v222, v223);
        let v225 = f64_min(v224, vec2<u32>(4294967295u, 1072693247u));
        let v226 = vec2<u32>(v222.x, v222.y & 0x7fffffffu);
        let v227 = !f64_eq(v226, vec2<u32>(0u, 2146435072u));
        let v228 = select(vec2<u32>(0u, 0u), v225, v227);
        let v229 = !(f64_is_nan(v222) | f64_is_nan(vec2<u32>(0u, 0u)));
        let v230 = select(v222, v228, v229);
        let v231 = f64_mul(v230, vec2<u32>(0u, 1083179008u));
        let v232 = f64_add(v209, v231);
        let v233 = f64_lt(v232, vec2<u32>(0u, 0u));
        let v234 = select(vec2<u32>(0u, 0u), vec2<u32>(0u, 1083179008u), v233);
        let v235 = f64_add(v231, v234);
        let v236 = f64_add(v209, v235);
        let v237 = f64_floor(v236);
        let v238 = f64_sub(v236, v237);
        let v239 = f64_min(v238, vec2<u32>(4294967295u, 1072693247u));
        let v240 = vec2<u32>(v236.x, v236.y & 0x7fffffffu);
        let v241 = !f64_eq(v240, vec2<u32>(0u, 2146435072u));
        let v242 = select(vec2<u32>(0u, 0u), v239, v241);
        let v243 = !(f64_is_nan(v236) | f64_is_nan(vec2<u32>(0u, 0u)));
        let v244 = select(v236, v242, v243);
        let v245 = f64_sub(v236, v244);
        let v246 = f64_to_f32(v245);
        let v247 = u32(i32(v246));
        let v248 = f64_sub(v235, v245);
        let v249 = f64_add(v209, v248);
        let v250 = f64_sub(v249, v248);
        let v251 = f64_sub(v209, v250);
        let v252 = f64_add(v216, v251);
        let v253 = f64_sub(v252, v251);
        let v254 = f64_sub(v216, v253);
        let v255 = f64_add(v221, v254);
        let v256 = (f64_lt(vec2<u32>(0u, 1071644672u), v249) | f64_eq(v249, vec2<u32>(0u, 1071644672u)));
        let v257 = select(0u, 1u, v256);
        let v258 = v257 + v247;
        let v259 = f64_from_u64(vec2<u32>(select(0u, 1u, v256), 0u));
        let v260 = f64_sub(v249, v259);
        let v261 = f64_add(v260, v252);
        let v262 = f64_sub(v261, v260);
        let v263 = f64_sub(v252, v262);
        let v264 = f64_add(v263, v255);
        let v265 = f64_mul(v261, vec2<u32>(1413754136u, 1073291771u));
        let v266 = vec2<u32>(v265.x, v265.y ^ 0x80000000u);
        let v267 = f64_fma(v261, vec2<u32>(1413754136u, 1073291771u), v266);
        let v268 = f64_fma(v261, vec2<u32>(856972294u, 1016178214u), v267);
        let v269 = f64_fma(v264, vec2<u32>(1413754136u, 1073291771u), v268);
        let v270 = f64_add(v265, v269);
        let v271 = f64_sub(v270, v265);
        let v272 = f64_sub(v269, v271);
        let v273 = select(v33, v270, v8);
        let v274 = select(v36, v272, v8);
        let v275 = select(v38, v258, v8);
        let v276 = f64_mul(v273, v273);
        let v277 = f64_mul(v276, vec2<u32>(0u, 1071644672u));
        let v278 = f64_sub(vec2<u32>(0u, 1072693248u), v277);
        let v279 = f64_fma(v276, vec2<u32>(4188289976u, 1038475442u), vec2<u32>(3023036327u, 3193628160u));
        let v280 = f64_fma(v279, v276, vec2<u32>(2037177857u, 1053236707u));
        let v281 = f64_fma(v280, v276, vec2<u32>(434650716u, 3207201184u));
        let v282 = f64_fma(v281, v276, vec2<u32>(286329779u, 1065423121u));
        let v283 = f64_fma(v276, vec2<u32>(1187798594u, 3181971419u), vec2<u32>(2419567480u, 1042411190u));
        let v284 = f64_fma(v283, v276, vec2<u32>(2709480950u, 3197271631u));
        let v285 = f64_fma(v284, v276, vec2<u32>(435481744u, 1056571808u));
        let v286 = f64_fma(v285, v276, vec2<u32>(381774183u, 3210133868u));
        let v287 = f64_fma(v286, v276, vec2<u32>(1431655765u, 1067799893u));
        let v288 = f64_mul(v276, v276);
        let v289 = f64_sub(vec2<u32>(0u, 1072693248u), v278);
        let v290 = f64_sub(v289, v277);
        let v291 = f64_fma(v273, v274, v290);
        let v292 = f64_fma(v287, v288, v291);
        let v293 = f64_add(v278, v292);
        let v294 = vec2<u32>(v273.x, v273.y ^ 0x80000000u);
        let v295 = f64_mul(v276, v294);
        let v296 = f64_mul(v274, vec2<u32>(0u, 1071644672u));
        let v297 = f64_fma(v295, v282, v296);
        let v298 = vec2<u32>(v274.x, v274.y ^ 0x80000000u);
        let v299 = f64_fma(v297, v276, v298);
        let v300 = f64_fma(v295, vec2<u32>(1431655765u, 3217380693u), v299);
        let v301 = f64_sub(v273, v300);
        let v302 = v275 & 1u;
        let v303 = v302 == 0u;
        let v304 = select(v293, v301, v303);
        let v305 = v304;
        let v306 = v275 >> 1u;
        let v307 = v306 & 1u;
        let v308 = f64_lt(v6, vec2<u32>(0u, 0u));
        let v309 = select(0u, 1u, v308);
        let v310 = v307 ^ v309;
        let v311 = vec2<u32>(v310, 0u);
        let v312 = shl64(v311, vec2<u32>(63u, 0u).x);
        let v313 = v312 ^ v305;
        let v314 = (f64_lt(v7, vec2<u32>(0u, 2146435072u)) | f64_lt(vec2<u32>(0u, 2146435072u), v7));
        let v315 = v313;
        let v316 = select(vec2<u32>(0u, 2146959360u), v315, v314);
        let v317 = a1 + v4 * 8u;
        buf1[((v317) - 4096u) >> 2u] = v316.x;
        buf1[(((v317 + 4u)) - 4096u) >> 2u] = v316.y;
        let v318 = v4 + 1u;
        let v319 = a0 + v318 * 8u;
        let v320 = vec2<u32>(buf0[((v319) - 4096u) >> 2u], buf0[(((v319 + 4u)) - 4096u) >> 2u]);
        let v321 = vec2<u32>(v320.x, v320.y & 0x7fffffffu);
        let v322 = !f64_lt(v321, vec2<u32>(0u, 1121976320u));
        let v323 = f64_fma(v321, vec2<u32>(1841940611u, 1071931184u), vec2<u32>(0u, 1071644672u));
        let v324 = f64_trunc(v323);
        let v325 = f64_mul(v324, vec2<u32>(1413754136u, 3220775419u));
        let v326 = vec2<u32>(v325.x, v325.y ^ 0x80000000u);
        let v327 = f64_fma(v324, vec2<u32>(1413754136u, 3220775419u), v326);
        let v328 = f64_mul(v324, vec2<u32>(856972294u, 3163661862u));
        let v329 = vec2<u32>(v328.x, v328.y ^ 0x80000000u);
        let v330 = f64_fma(v324, vec2<u32>(856972294u, 3163661862u), v329);
        let v331 = f64_mul(v324, vec2<u32>(688016904u, 3108773073u));
        let v332 = vec2<u32>(v331.x, v331.y ^ 0x80000000u);
        let v333 = f64_fma(v324, vec2<u32>(688016904u, 3108773073u), v332);
        let v334 = f64_add(v328, v327);
        let v335 = f64_sub(v334, v327);
        let v336 = f64_sub(v328, v335);
        let v337 = f64_add(v331, v336);
        let v338 = f64_add(v330, v337);
        let v339 = f64_add(v333, v338);
        let v340 = f64_add(v325, v334);
        let v341 = f64_sub(v340, v325);
        let v342 = f64_sub(v334, v341);
        let v343 = f64_add(v342, v339);
        let v344 = f64_sub(v343, v342);
        let v345 = f64_sub(v339, v344);
        let v346 = f64_add(v321, v340);
        let v347 = f64_add(v346, v343);
        let v348 = f64_sub(v347, v346);
        let v349 = f64_sub(v343, v348);
        let v350 = f64_add(v345, v349);
        let v351 = f64_to_i64(v324);
        let v352 = v351.x;
        let v353 = v321;
        let v354 = lshr64(v353, vec2<u32>(52u, 0u).x);
        let v355 = v354.x;
        let v356 = v355 + 4294966273u;
        let v357 = u32(bitcast<i32>(v356) >> 3u);
        let v358 = u32(max(bitcast<i32>(v357), bitcast<i32>(6u)));
        let v359 = 133u - v358;
        let v360 = v359 & 240u;
        let v361 = 4096u + v360;
        let v362 = vec2<u32>(glob0[((v361) - 4096u) >> 2u], glob0[(((v361 + 4u)) - 4096u) >> 2u]);
        let v363 = v361 + 8u;
        let v364 = vec2<u32>(glob0[((v363) - 4096u) >> 2u], glob0[(((v363 + 4u)) - 4096u) >> 2u]);
        let v365 = v361 + 16u;
        let v366 = vec2<u32>(glob0[((v365) - 4096u) >> 2u], glob0[(((v365 + 4u)) - 4096u) >> 2u]);
        let v367 = v361 + 24u;
        let v368 = vec2<u32>(glob0[((v367) - 4096u) >> 2u], glob0[(((v367 + 4u)) - 4096u) >> 2u]);
        let v369 = v361 + 32u;
        let v370 = vec2<u32>(glob0[((v369) - 4096u) >> 2u], glob0[(((v369 + 4u)) - 4096u) >> 2u]);
        let v371 = v362.x;
        let v372 = lshr64(v362, vec2<u32>(32u, 0u).x);
        let v373 = v372.x;
        let v374 = v364.x;
        let v375 = lshr64(v364, vec2<u32>(32u, 0u).x);
        let v376 = v375.x;
        let v377 = v366.x;
        let v378 = lshr64(v366, vec2<u32>(32u, 0u).x);
        let v379 = v378.x;
        let v380 = v368.x;
        let v381 = lshr64(v368, vec2<u32>(32u, 0u).x);
        let v382 = v381.x;
        let v383 = v370.x;
        let v384 = lshr64(v370, vec2<u32>(32u, 0u).x);
        let v385 = v384.x;
        let v386 = v359 >> 2u;
        let v387 = v386 & 3u;
        let v388 = v387 == 1u;
        let v389 = v387 == 2u;
        let v390 = v387 == 3u;
        let v391 = select(v371, v373, v388);
        let v392 = select(v391, v374, v389);
        let v393 = select(v392, v376, v390);
        let v394 = select(v373, v374, v388);
        let v395 = select(v394, v376, v389);
        let v396 = select(v395, v377, v390);
        let v397 = select(v374, v376, v388);
        let v398 = select(v397, v377, v389);
        let v399 = select(v398, v379, v390);
        let v400 = select(v376, v377, v388);
        let v401 = select(v400, v379, v389);
        let v402 = select(v401, v380, v390);
        let v403 = select(v377, v379, v388);
        let v404 = select(v403, v380, v389);
        let v405 = select(v404, v382, v390);
        let v406 = select(v379, v380, v388);
        let v407 = select(v406, v382, v389);
        let v408 = select(v407, v383, v390);
        let v409 = select(v380, v382, v388);
        let v410 = select(v409, v383, v389);
        let v411 = select(v410, v385, v390);
        let v412 = vec2<u32>(v396, 0u);
        let v413 = shl64(v412, vec2<u32>(32u, 0u).x);
        let v414 = vec2<u32>(v393, 0u);
        let v415 = v413 | v414;
        let v416 = v359 << 3u;
        let v417 = v416 & 24u;
        let v418 = vec2<u32>(v417, 0u);
        let v419 = ashr64(v415, v418.x);
        let v420 = vec2<u32>(v399, 0u);
        let v421 = shl64(v420, vec2<u32>(32u, 0u).x);
        let v422 = v421 | v412;
        let v423 = ashr64(v422, v418.x);
        let v424 = v423.x;
        let v425 = vec2<u32>(v402, 0u);
        let v426 = shl64(v425, vec2<u32>(32u, 0u).x);
        let v427 = v426 | v420;
        let v428 = ashr64(v427, v418.x);
        let v429 = v428.x;
        let v430 = vec2<u32>(v405, 0u);
        let v431 = shl64(v430, vec2<u32>(32u, 0u).x);
        let v432 = v431 | v425;
        let v433 = ashr64(v432, v418.x);
        let v434 = vec2<u32>(v408, 0u);
        let v435 = shl64(v434, vec2<u32>(32u, 0u).x);
        let v436 = v435 | v430;
        let v437 = ashr64(v436, v418.x);
        let v438 = v437.x;
        let v439 = vec2<u32>(v411, 0u);
        let v440 = shl64(v439, vec2<u32>(32u, 0u).x);
        let v441 = v440 | v434;
        let v442 = ashr64(v441, v418.x);
        let v443 = v442.x;
        let v444 = bitcast<i32>(v357) < bitcast<i32>(111u);
        let v445 = select(vec2<u32>(0u, 930086912u), vec2<u32>(0u, 1072693248u), v444);
        let v446 = f64_mul(v321, v445);
        let v447 = select(142606336u, 0u, v444);
        let v448 = v358 << 23u;
        let v449 = v447 - v448;
        let v450 = v449 + 982515712u;
        let v451 = vec2<u32>(v450, 0u);
        let v452 = shl64(v451, vec2<u32>(32u, 0u).x);
        let v453 = v452;
        let v454 = v419 & vec2<u32>(4294967295u, 0u);
        let v455 = v424 & 65535u;
        let v456 = v455 | v450;
        let v457 = vec2<u32>(v456, 0u);
        let v458 = shl64(v457, vec2<u32>(32u, 0u).x);
        let v459 = v458 | v454;
        let v460 = v459;
        let v461 = f64_sub(v460, v453);
        let v462 = v449 + 1032847360u;
        let v463 = vec2<u32>(v462, 0u);
        let v464 = shl64(v463, vec2<u32>(32u, 0u).x);
        let v465 = v464;
        let v466 = select(((v429 << (16u % 32u)) | (v424 >> (32u - (16u % 32u)))) & 4294967295u, v429, (16u % 32u) == 0u);
        let v467 = vec2<u32>(v466, 0u);
        let v468 = v429 >> 16u;
        let v469 = v468 | v462;
        let v470 = vec2<u32>(v469, 0u);
        let v471 = shl64(v470, vec2<u32>(32u, 0u).x);
        let v472 = v471 | v467;
        let v473 = v472;
        let v474 = f64_sub(v473, v465);
        let v475 = v449 + 1083179008u;
        let v476 = vec2<u32>(v475, 0u);
        let v477 = v433 & vec2<u32>(4294967295u, 0u);
        let v478 = v438 & 65535u;
        let v479 = v478 | v475;
        let v480 = vec2<u32>(v479, 0u);
        let v481 = v449 + 1133510656u;
        let v482 = vec2<u32>(v481, 0u);
        let v483 = select(((v443 << (16u % 32u)) | (v438 >> (32u - (16u % 32u)))) & 4294967295u, v443, (16u % 32u) == 0u);
        let v484 = vec2<u32>(v483, 0u);
        let v485 = v443 >> 16u;
        let v486 = v485 | v481;
        let v487 = vec2<u32>(v486, 0u);
        let v488 = f64_mul(v446, v461);
        let v489 = vec2<u32>(v488.x, v488.y ^ 0x80000000u);
        let v490 = f64_fma(v461, v446, v489);
        let v491 = f64_mul(v446, v474);
        let v492 = vec2<u32>(v491.x, v491.y ^ 0x80000000u);
        let v493 = f64_fma(v474, v446, v492);
        let v494 = array<vec2<u32>, 2>(v476, array<vec2<u32>, 2>()[1]);
        let v495 = array<vec2<u32>, 2>(v494[0], v482);
        v496[0] = shl64(v495[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
        v496[1] = shl64(v495[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
        v497[0] = v496[0];
        v497[1] = v496[1];
        let v498 = array<vec2<u32>, 2>(v480, array<vec2<u32>, 2>()[1]);
        let v499 = array<vec2<u32>, 2>(v498[0], v487);
        v500[0] = shl64(v499[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
        v500[1] = shl64(v499[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
        let v501 = array<vec2<u32>, 2>(v477, array<vec2<u32>, 2>()[1]);
        let v502 = array<vec2<u32>, 2>(v501[0], v484);
        v503[0] = v500[0] | v502[0];
        v503[1] = v500[1] | v502[1];
        v504[0] = v503[0];
        v504[1] = v503[1];
        v505[0] = f64_sub(v504[0], v497[0]);
        v505[1] = f64_sub(v504[1], v497[1]);
        let v506 = array<vec2<u32>, 2>(v446, array<vec2<u32>, 2>()[1]);
        let v507 = array<vec2<u32>, 2>(v506[0], v506[0]);
        v508[0] = f64_mul(v507[0], v505[0]);
        v508[1] = f64_mul(v507[1], v505[1]);
        let v509 = v508[0];
        let v510 = vec2<u32>(v509.x, v509.y ^ 0x80000000u);
        let v511 = v505[0];
        let v512 = f64_fma(v511, v446, v510);
        let v513 = v508[1];
        let v514 = vec2<u32>(v513.x, v513.y ^ 0x80000000u);
        let v515 = v505[1];
        let v516 = f64_fma(v515, v446, v514);
        let v517 = f64_add(v509, v513);
        let v518 = f64_sub(v517, v513);
        let v519 = f64_sub(v509, v518);
        let v520 = f64_add(v516, v519);
        let v521 = f64_sub(v520, v516);
        let v522 = f64_sub(v519, v521);
        let v523 = f64_add(v491, v520);
        let v524 = f64_sub(v523, v520);
        let v525 = f64_sub(v491, v524);
        let v526 = f64_add(v522, v525);
        let v527 = f64_add(v512, v526);
        let v528 = f64_sub(v527, v512);
        let v529 = f64_sub(v526, v528);
        let v530 = f64_add(v488, v527);
        let v531 = f64_sub(v530, v527);
        let v532 = f64_sub(v488, v531);
        let v533 = f64_add(v529, v532);
        let v534 = f64_add(v493, v533);
        let v535 = f64_add(v490, v534);
        let v536 = f64_mul(v517, vec2<u32>(0u, 1062207488u));
        let v537 = f64_floor(v536);
        let v538 = f64_sub(v536, v537);
        let v539 = f64_min(v538, vec2<u32>(4294967295u, 1072693247u));
        let v540 = vec2<u32>(v536.x, v536.y & 0x7fffffffu);
        let v541 = !f64_eq(v540, vec2<u32>(0u, 2146435072u));
        let v542 = select(vec2<u32>(0u, 0u), v539, v541);
        let v543 = !(f64_is_nan(v536) | f64_is_nan(vec2<u32>(0u, 0u)));
        let v544 = select(v536, v542, v543);
        let v545 = f64_mul(v544, vec2<u32>(0u, 1083179008u));
        let v546 = f64_add(v523, v545);
        let v547 = f64_lt(v546, vec2<u32>(0u, 0u));
        let v548 = select(vec2<u32>(0u, 0u), vec2<u32>(0u, 1083179008u), v547);
        let v549 = f64_add(v545, v548);
        let v550 = f64_add(v523, v549);
        let v551 = f64_floor(v550);
        let v552 = f64_sub(v550, v551);
        let v553 = f64_min(v552, vec2<u32>(4294967295u, 1072693247u));
        let v554 = vec2<u32>(v550.x, v550.y & 0x7fffffffu);
        let v555 = !f64_eq(v554, vec2<u32>(0u, 2146435072u));
        let v556 = select(vec2<u32>(0u, 0u), v553, v555);
        let v557 = !(f64_is_nan(v550) | f64_is_nan(vec2<u32>(0u, 0u)));
        let v558 = select(v550, v556, v557);
        let v559 = f64_sub(v550, v558);
        let v560 = f64_to_f32(v559);
        let v561 = u32(i32(v560));
        let v562 = f64_sub(v549, v559);
        let v563 = f64_add(v523, v562);
        let v564 = f64_sub(v563, v562);
        let v565 = f64_sub(v523, v564);
        let v566 = f64_add(v530, v565);
        let v567 = f64_sub(v566, v565);
        let v568 = f64_sub(v530, v567);
        let v569 = f64_add(v535, v568);
        let v570 = (f64_lt(vec2<u32>(0u, 1071644672u), v563) | f64_eq(v563, vec2<u32>(0u, 1071644672u)));
        let v571 = select(0u, 1u, v570);
        let v572 = v571 + v561;
        let v573 = f64_from_u64(vec2<u32>(select(0u, 1u, v570), 0u));
        let v574 = f64_sub(v563, v573);
        let v575 = f64_add(v574, v566);
        let v576 = f64_sub(v575, v574);
        let v577 = f64_sub(v566, v576);
        let v578 = f64_add(v577, v569);
        let v579 = f64_mul(v575, vec2<u32>(1413754136u, 1073291771u));
        let v580 = vec2<u32>(v579.x, v579.y ^ 0x80000000u);
        let v581 = f64_fma(v575, vec2<u32>(1413754136u, 1073291771u), v580);
        let v582 = f64_fma(v575, vec2<u32>(856972294u, 1016178214u), v581);
        let v583 = f64_fma(v578, vec2<u32>(1413754136u, 1073291771u), v582);
        let v584 = f64_add(v579, v583);
        let v585 = f64_sub(v584, v579);
        let v586 = f64_sub(v583, v585);
        let v587 = select(v347, v584, v322);
        let v588 = select(v350, v586, v322);
        let v589 = select(v352, v572, v322);
        let v590 = f64_mul(v587, v587);
        let v591 = f64_mul(v590, vec2<u32>(0u, 1071644672u));
        let v592 = f64_sub(vec2<u32>(0u, 1072693248u), v591);
        let v593 = f64_fma(v590, vec2<u32>(4188289976u, 1038475442u), vec2<u32>(3023036327u, 3193628160u));
        let v594 = f64_fma(v593, v590, vec2<u32>(2037177857u, 1053236707u));
        let v595 = f64_fma(v594, v590, vec2<u32>(434650716u, 3207201184u));
        let v596 = f64_fma(v595, v590, vec2<u32>(286329779u, 1065423121u));
        let v597 = f64_fma(v590, vec2<u32>(1187798594u, 3181971419u), vec2<u32>(2419567480u, 1042411190u));
        let v598 = f64_fma(v597, v590, vec2<u32>(2709480950u, 3197271631u));
        let v599 = f64_fma(v598, v590, vec2<u32>(435481744u, 1056571808u));
        let v600 = f64_fma(v599, v590, vec2<u32>(381774183u, 3210133868u));
        let v601 = f64_fma(v600, v590, vec2<u32>(1431655765u, 1067799893u));
        let v602 = f64_mul(v590, v590);
        let v603 = f64_sub(vec2<u32>(0u, 1072693248u), v592);
        let v604 = f64_sub(v603, v591);
        let v605 = f64_fma(v587, v588, v604);
        let v606 = f64_fma(v601, v602, v605);
        let v607 = f64_add(v592, v606);
        let v608 = vec2<u32>(v587.x, v587.y ^ 0x80000000u);
        let v609 = f64_mul(v590, v608);
        let v610 = f64_mul(v588, vec2<u32>(0u, 1071644672u));
        let v611 = f64_fma(v609, v596, v610);
        let v612 = vec2<u32>(v588.x, v588.y ^ 0x80000000u);
        let v613 = f64_fma(v611, v590, v612);
        let v614 = f64_fma(v609, vec2<u32>(1431655765u, 3217380693u), v613);
        let v615 = f64_sub(v587, v614);
        let v616 = v589 & 1u;
        let v617 = v616 == 0u;
        let v618 = select(v607, v615, v617);
        let v619 = v618;
        let v620 = v589 >> 1u;
        let v621 = v620 & 1u;
        let v622 = f64_lt(v320, vec2<u32>(0u, 0u));
        let v623 = select(0u, 1u, v622);
        let v624 = v621 ^ v623;
        let v625 = vec2<u32>(v624, 0u);
        let v626 = shl64(v625, vec2<u32>(63u, 0u).x);
        let v627 = v626 ^ v619;
        let v628 = (f64_lt(v321, vec2<u32>(0u, 2146435072u)) | f64_lt(vec2<u32>(0u, 2146435072u), v321));
        let v629 = v627;
        let v630 = select(vec2<u32>(0u, 2146959360u), v629, v628);
        let v631 = a1 + v318 * 8u;
        buf1[((v631) - 4096u) >> 2u] = v630.x;
        buf1[(((v631 + 4u)) - 4096u) >> 2u] = v630.y;
        let v632 = v4 + 2u;
        let v633 = a0 + v632 * 8u;
        let v634 = vec2<u32>(buf0[((v633) - 4096u) >> 2u], buf0[(((v633 + 4u)) - 4096u) >> 2u]);
        let v635 = vec2<u32>(v634.x, v634.y & 0x7fffffffu);
        let v636 = !f64_lt(v635, vec2<u32>(0u, 1121976320u));
        let v637 = f64_fma(v635, vec2<u32>(1841940611u, 1071931184u), vec2<u32>(0u, 1071644672u));
        let v638 = f64_trunc(v637);
        let v639 = f64_mul(v638, vec2<u32>(1413754136u, 3220775419u));
        let v640 = vec2<u32>(v639.x, v639.y ^ 0x80000000u);
        let v641 = f64_fma(v638, vec2<u32>(1413754136u, 3220775419u), v640);
        let v642 = f64_mul(v638, vec2<u32>(856972294u, 3163661862u));
        let v643 = vec2<u32>(v642.x, v642.y ^ 0x80000000u);
        let v644 = f64_fma(v638, vec2<u32>(856972294u, 3163661862u), v643);
        let v645 = f64_mul(v638, vec2<u32>(688016904u, 3108773073u));
        let v646 = vec2<u32>(v645.x, v645.y ^ 0x80000000u);
        let v647 = f64_fma(v638, vec2<u32>(688016904u, 3108773073u), v646);
        let v648 = f64_add(v642, v641);
        let v649 = f64_sub(v648, v641);
        let v650 = f64_sub(v642, v649);
        let v651 = f64_add(v645, v650);
        let v652 = f64_add(v644, v651);
        let v653 = f64_add(v647, v652);
        let v654 = f64_add(v639, v648);
        let v655 = f64_sub(v654, v639);
        let v656 = f64_sub(v648, v655);
        let v657 = f64_add(v656, v653);
        let v658 = f64_sub(v657, v656);
        let v659 = f64_sub(v653, v658);
        let v660 = f64_add(v635, v654);
        let v661 = f64_add(v660, v657);
        let v662 = f64_sub(v661, v660);
        let v663 = f64_sub(v657, v662);
        let v664 = f64_add(v659, v663);
        let v665 = f64_to_i64(v638);
        let v666 = v665.x;
        let v667 = v635;
        let v668 = lshr64(v667, vec2<u32>(52u, 0u).x);
        let v669 = v668.x;
        let v670 = v669 + 4294966273u;
        let v671 = u32(bitcast<i32>(v670) >> 3u);
        let v672 = u32(max(bitcast<i32>(v671), bitcast<i32>(6u)));
        let v673 = 133u - v672;
        let v674 = v673 & 240u;
        let v675 = 4096u + v674;
        let v676 = vec2<u32>(glob0[((v675) - 4096u) >> 2u], glob0[(((v675 + 4u)) - 4096u) >> 2u]);
        let v677 = v675 + 8u;
        let v678 = vec2<u32>(glob0[((v677) - 4096u) >> 2u], glob0[(((v677 + 4u)) - 4096u) >> 2u]);
        let v679 = v675 + 16u;
        let v680 = vec2<u32>(glob0[((v679) - 4096u) >> 2u], glob0[(((v679 + 4u)) - 4096u) >> 2u]);
        let v681 = v675 + 24u;
        let v682 = vec2<u32>(glob0[((v681) - 4096u) >> 2u], glob0[(((v681 + 4u)) - 4096u) >> 2u]);
        let v683 = v675 + 32u;
        let v684 = vec2<u32>(glob0[((v683) - 4096u) >> 2u], glob0[(((v683 + 4u)) - 4096u) >> 2u]);
        let v685 = v676.x;
        let v686 = lshr64(v676, vec2<u32>(32u, 0u).x);
        let v687 = v686.x;
        let v688 = v678.x;
        let v689 = lshr64(v678, vec2<u32>(32u, 0u).x);
        let v690 = v689.x;
        let v691 = v680.x;
        let v692 = lshr64(v680, vec2<u32>(32u, 0u).x);
        let v693 = v692.x;
        let v694 = v682.x;
        let v695 = lshr64(v682, vec2<u32>(32u, 0u).x);
        let v696 = v695.x;
        let v697 = v684.x;
        let v698 = lshr64(v684, vec2<u32>(32u, 0u).x);
        let v699 = v698.x;
        let v700 = v673 >> 2u;
        let v701 = v700 & 3u;
        let v702 = v701 == 1u;
        let v703 = v701 == 2u;
        let v704 = v701 == 3u;
        let v705 = select(v685, v687, v702);
        let v706 = select(v705, v688, v703);
        let v707 = select(v706, v690, v704);
        let v708 = select(v687, v688, v702);
        let v709 = select(v708, v690, v703);
        let v710 = select(v709, v691, v704);
        let v711 = select(v688, v690, v702);
        let v712 = select(v711, v691, v703);
        let v713 = select(v712, v693, v704);
        let v714 = select(v690, v691, v702);
        let v715 = select(v714, v693, v703);
        let v716 = select(v715, v694, v704);
        let v717 = select(v691, v693, v702);
        let v718 = select(v717, v694, v703);
        let v719 = select(v718, v696, v704);
        let v720 = select(v693, v694, v702);
        let v721 = select(v720, v696, v703);
        let v722 = select(v721, v697, v704);
        let v723 = select(v694, v696, v702);
        let v724 = select(v723, v697, v703);
        let v725 = select(v724, v699, v704);
        let v726 = vec2<u32>(v710, 0u);
        let v727 = shl64(v726, vec2<u32>(32u, 0u).x);
        let v728 = vec2<u32>(v707, 0u);
        let v729 = v727 | v728;
        let v730 = v673 << 3u;
        let v731 = v730 & 24u;
        let v732 = vec2<u32>(v731, 0u);
        let v733 = ashr64(v729, v732.x);
        let v734 = vec2<u32>(v713, 0u);
        let v735 = shl64(v734, vec2<u32>(32u, 0u).x);
        let v736 = v735 | v726;
        let v737 = ashr64(v736, v732.x);
        let v738 = v737.x;
        let v739 = vec2<u32>(v716, 0u);
        let v740 = shl64(v739, vec2<u32>(32u, 0u).x);
        let v741 = v740 | v734;
        let v742 = ashr64(v741, v732.x);
        let v743 = v742.x;
        let v744 = vec2<u32>(v719, 0u);
        let v745 = shl64(v744, vec2<u32>(32u, 0u).x);
        let v746 = v745 | v739;
        let v747 = ashr64(v746, v732.x);
        let v748 = vec2<u32>(v722, 0u);
        let v749 = shl64(v748, vec2<u32>(32u, 0u).x);
        let v750 = v749 | v744;
        let v751 = ashr64(v750, v732.x);
        let v752 = v751.x;
        let v753 = vec2<u32>(v725, 0u);
        let v754 = shl64(v753, vec2<u32>(32u, 0u).x);
        let v755 = v754 | v748;
        let v756 = ashr64(v755, v732.x);
        let v757 = v756.x;
        let v758 = bitcast<i32>(v671) < bitcast<i32>(111u);
        let v759 = select(vec2<u32>(0u, 930086912u), vec2<u32>(0u, 1072693248u), v758);
        let v760 = f64_mul(v635, v759);
        let v761 = select(142606336u, 0u, v758);
        let v762 = v672 << 23u;
        let v763 = v761 - v762;
        let v764 = v763 + 982515712u;
        let v765 = vec2<u32>(v764, 0u);
        let v766 = shl64(v765, vec2<u32>(32u, 0u).x);
        let v767 = v766;
        let v768 = v733 & vec2<u32>(4294967295u, 0u);
        let v769 = v738 & 65535u;
        let v770 = v769 | v764;
        let v771 = vec2<u32>(v770, 0u);
        let v772 = shl64(v771, vec2<u32>(32u, 0u).x);
        let v773 = v772 | v768;
        let v774 = v773;
        let v775 = f64_sub(v774, v767);
        let v776 = v763 + 1032847360u;
        let v777 = vec2<u32>(v776, 0u);
        let v778 = shl64(v777, vec2<u32>(32u, 0u).x);
        let v779 = v778;
        let v780 = select(((v743 << (16u % 32u)) | (v738 >> (32u - (16u % 32u)))) & 4294967295u, v743, (16u % 32u) == 0u);
        let v781 = vec2<u32>(v780, 0u);
        let v782 = v743 >> 16u;
        let v783 = v782 | v776;
        let v784 = vec2<u32>(v783, 0u);
        let v785 = shl64(v784, vec2<u32>(32u, 0u).x);
        let v786 = v785 | v781;
        let v787 = v786;
        let v788 = f64_sub(v787, v779);
        let v789 = v763 + 1083179008u;
        let v790 = vec2<u32>(v789, 0u);
        let v791 = v747 & vec2<u32>(4294967295u, 0u);
        let v792 = v752 & 65535u;
        let v793 = v792 | v789;
        let v794 = vec2<u32>(v793, 0u);
        let v795 = v763 + 1133510656u;
        let v796 = vec2<u32>(v795, 0u);
        let v797 = select(((v757 << (16u % 32u)) | (v752 >> (32u - (16u % 32u)))) & 4294967295u, v757, (16u % 32u) == 0u);
        let v798 = vec2<u32>(v797, 0u);
        let v799 = v757 >> 16u;
        let v800 = v799 | v795;
        let v801 = vec2<u32>(v800, 0u);
        let v802 = f64_mul(v760, v775);
        let v803 = vec2<u32>(v802.x, v802.y ^ 0x80000000u);
        let v804 = f64_fma(v775, v760, v803);
        let v805 = f64_mul(v760, v788);
        let v806 = vec2<u32>(v805.x, v805.y ^ 0x80000000u);
        let v807 = f64_fma(v788, v760, v806);
        let v808 = array<vec2<u32>, 2>(v790, array<vec2<u32>, 2>()[1]);
        let v809 = array<vec2<u32>, 2>(v808[0], v796);
        v810[0] = shl64(v809[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
        v810[1] = shl64(v809[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
        v811[0] = v810[0];
        v811[1] = v810[1];
        let v812 = array<vec2<u32>, 2>(v794, array<vec2<u32>, 2>()[1]);
        let v813 = array<vec2<u32>, 2>(v812[0], v801);
        v814[0] = shl64(v813[0], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[0].x);
        v814[1] = shl64(v813[1], array<vec2<u32>, 2>(vec2<u32>(32u, 0u), vec2<u32>(32u, 0u))[1].x);
        let v815 = array<vec2<u32>, 2>(v791, array<vec2<u32>, 2>()[1]);
        let v816 = array<vec2<u32>, 2>(v815[0], v798);
        v817[0] = v814[0] | v816[0];
        v817[1] = v814[1] | v816[1];
        v818[0] = v817[0];
        v818[1] = v817[1];
        v819[0] = f64_sub(v818[0], v811[0]);
        v819[1] = f64_sub(v818[1], v811[1]);
        let v820 = array<vec2<u32>, 2>(v760, array<vec2<u32>, 2>()[1]);
        let v821 = array<vec2<u32>, 2>(v820[0], v820[0]);
        v822[0] = f64_mul(v821[0], v819[0]);
        v822[1] = f64_mul(v821[1], v819[1]);
        let v823 = v822[0];
        let v824 = vec2<u32>(v823.x, v823.y ^ 0x80000000u);
        let v825 = v819[0];
        let v826 = f64_fma(v825, v760, v824);
        let v827 = v822[1];
        let v828 = vec2<u32>(v827.x, v827.y ^ 0x80000000u);
        let v829 = v819[1];
        let v830 = f64_fma(v829, v760, v828);
        let v831 = f64_add(v823, v827);
        let v832 = f64_sub(v831, v827);
        let v833 = f64_sub(v823, v832);
        let v834 = f64_add(v830, v833);
        let v835 = f64_sub(v834, v830);
        let v836 = f64_sub(v833, v835);
        let v837 = f64_add(v805, v834);
        let v838 = f64_sub(v837, v834);
        let v839 = f64_sub(v805, v838);
        let v840 = f64_add(v836, v839);
        let v841 = f64_add(v826, v840);
        let v842 = f64_sub(v841, v826);
        let v843 = f64_sub(v840, v842);
        let v844 = f64_add(v802, v841);
        let v845 = f64_sub(v844, v841);
        let v846 = f64_sub(v802, v845);
        let v847 = f64_add(v843, v846);
        let v848 = f64_add(v807, v847);
        let v849 = f64_add(v804, v848);
        let v850 = f64_mul(v831, vec2<u32>(0u, 1062207488u));
        let v851 = f64_floor(v850);
        let v852 = f64_sub(v850, v851);
        let v853 = f64_min(v852, vec2<u32>(4294967295u, 1072693247u));
        let v854 = vec2<u32>(v850.x, v850.y & 0x7fffffffu);
        let v855 = !f64_eq(v854, vec2<u32>(0u, 2146435072u));
        let v856 = select(vec2<u32>(0u, 0u), v853, v855);
        let v857 = !(f64_is_nan(v850) | f64_is_nan(vec2<u32>(0u, 0u)));
        let v858 = select(v850, v856, v857);
        let v859 = f64_mul(v858, vec2<u32>(0u, 1083179008u));
        let v860 = f64_add(v837, v859);
        let v861 = f64_lt(v860, vec2<u32>(0u, 0u));
        let v862 = select(vec2<u32>(0u, 0u), vec2<u32>(0u, 1083179008u), v861);
        let v863 = f64_add(v859, v862);
        let v864 = f64_add(v837, v863);
        let v865 = f64_floor(v864);
        let v866 = f64_sub(v864, v865);
        let v867 = f64_min(v866, vec2<u32>(4294967295u, 1072693247u));
        let v868 = vec2<u32>(v864.x, v864.y & 0x7fffffffu);
        let v869 = !f64_eq(v868, vec2<u32>(0u, 2146435072u));
        let v870 = select(vec2<u32>(0u, 0u), v867, v869);
        let v871 = !(f64_is_nan(v864) | f64_is_nan(vec2<u32>(0u, 0u)));
        let v872 = select(v864, v870, v871);
        let v873 = f64_sub(v864, v872);
        let v874 = f64_to_f32(v873);
        let v875 = u32(i32(v874));
        let v876 = f64_sub(v863, v873);
        let v877 = f64_add(v837, v876);
        let v878 = f64_sub(v877, v876);
        let v879 = f64_sub(v837, v878);
        let v880 = f64_add(v844, v879);
        let v881 = f64_sub(v880, v879);
        let v882 = f64_sub(v844, v881);
        let v883 = f64_add(v849, v882);
        let v884 = (f64_lt(vec2<u32>(0u, 1071644672u), v877) | f64_eq(v877, vec2<u32>(0u, 1071644672u)));
        let v885 = select(0u, 1u, v884);
        let v886 = v885 + v875;
        let v887 = f64_from_u64(vec2<u32>(select(0u, 1u, v884), 0u));
        let v888 = f64_sub(v877, v887);
        let v889 = f64_add(v888, v880);
        let v890 = f64_sub(v889, v888);
        let v891 = f64_sub(v880, v890);
        let v892 = f64_add(v891, v883);
        let v893 = f64_mul(v889, vec2<u32>(1413754136u, 1073291771u));
        let v894 = vec2<u32>(v893.x, v893.y ^ 0x80000000u);
        let v895 = f64_fma(v889, vec2<u32>(1413754136u, 1073291771u), v894);
        let v896 = f64_fma(v889, vec2<u32>(856972294u, 1016178214u), v895);
        let v897 = f64_fma(v892, vec2<u32>(1413754136u, 1073291771u), v896);
        let v898 = f64_add(v893, v897);
        let v899 = f64_sub(v898, v893);
        let v900 = f64_sub(v897, v899);
        let v901 = select(v661, v898, v636);
        let v902 = select(v664, v900, v636);
        let v903 = select(v666, v886, v636);
        let v904 = f64_mul(v901, v901);
        let v905 = f64_mul(v904, vec2<u32>(0u, 1071644672u));
        let v906 = f64_sub(vec2<u32>(0u, 1072693248u), v905);
        let v907 = f64_fma(v904, vec2<u32>(4188289976u, 1038475442u), vec2<u32>(3023036327u, 3193628160u));
        let v908 = f64_fma(v907, v904, vec2<u32>(2037177857u, 1053236707u));
        let v909 = f64_fma(v908, v904, vec2<u32>(434650716u, 3207201184u));
        let v910 = f64_fma(v909, v904, vec2<u32>(286329779u, 1065423121u));
        let v911 = f64_fma(v904, vec2<u32>(1187798594u, 3181971419u), vec2<u32>(2419567480u, 1042411190u));
        let v912 = f64_fma(v911, v904, vec2<u32>(2709480950u, 3197271631u));
        let v913 = f64_fma(v912, v904, vec2<u32>(435481744u, 1056571808u));
        let v914 = f64_fma(v913, v904, vec2<u32>(381774183u, 3210133868u));
        let v915 = f64_fma(v914, v904, vec2<u32>(1431655765u, 1067799893u));
        let v916 = f64_mul(v904, v904);
        let v917 = f64_sub(vec2<u32>(0u, 1072693248u), v906);
        let v918 = f64_sub(v917, v905);
        let v919 = f64_fma(v901, v902, v918);
        let v920 = f64_fma(v915, v916, v919);
        let v921 = f64_add(v906, v920);
        let v922 = vec2<u32>(v901.x, v901.y ^ 0x80000000u);
        let v923 = f64_mul(v904, v922);
        let v924 = f64_mul(v902, vec2<u32>(0u, 1071644672u));
        let v925 = f64_fma(v923, v910, v924);
        let v926 = vec2<u32>(v902.x, v902.y ^ 0x80000000u);
        let v927 = f64_fma(v925, v904, v926);
        let v928 = f64_fma(v923, vec2<u32>(1431655765u, 3217380693u), v927);
        let v929 = f64_sub(v901, v928);
        let v930 = v903 & 1u;
        let v931 = v930 == 0u;
        let v932 = select(v921, v929, v931);
        let v933 = v932;
        let v934 = v903 >> 1u;
        let v935 = v934 & 1u;
        let v936 = f64_lt(v634, vec2<u32>(0u, 0u));
        let v937 = select(0u, 1u, v936);
        let v938 = v935 ^ v937;
        let v939 = vec2<u32>(v938, 0u);
        let v940 = shl64(v939, vec2<u32>(63u, 0u).x);
        let v941 = v940 ^ v933;
        let v942 = (f64_lt(v635, vec2<u32>(0u, 2146435072u)) | f64_lt(vec2<u32>(0u, 2146435072u), v635));
        let v943 = v941;
        let v944 = select(vec2<u32>(0u, 2146959360u), v943, v942);
        let v945 = a1 + v632 * 8u;
        buf1[((v945) - 4096u) >> 2u] = v944.x;
        buf1[(((v945 + 4u)) - 4096u) >> 2u] = v944.y;
    } else {
    }
    return;
}


