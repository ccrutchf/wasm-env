#pragma OPENCL EXTENSION cl_khr_fp64 : enable
__kernel void k(__global const double *a, __global const double *b, __global double *o, __global long *l) {
    int i = get_global_id(0);
    double x = a[i], y = b[i];
    __global double *r = o + i * 16;
    r[0] = x + y;
    r[1] = x * y - x / y;
    r[2] = fma(x, y, -1.0);
    r[3] = sqrt(fabs(x));
    r[4] = exp(y / 1e3);
    r[5] = sin(x);
    r[6] = pow(fabs(x), 0.5);
    r[7] = floor(x) + ceil(y) * 2.0;
    double2 v = (double2)(x, y) * (double2)(3.0, -0.25) + (double2)(1.0);
    r[8] = v.x;
    r[9] = v.y;
    r[10] = (double)(float)x;
    r[11] = (double)(i * 7 - 300);
    r[12] = fmax(x, y);
    r[13] = dot((double2)(x, y), (double2)(y, x));
    r[14] = x < y ? 1.0 : 2.0;
    r[15] = (double)convert_int_rtn(y);
    __global long *s = l + i * 4;
    s[0] = (long)x;
    s[1] = convert_long_sat(y * 1e10);
    s[2] = as_long(x);
    s[3] = isnan(x) + 2 * isless(x, y);
}
