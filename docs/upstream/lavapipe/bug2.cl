#pragma OPENCL EXTENSION cl_khr_fp64 : enable
__kernel void k(__global const double *z, __global double *o, __global const double *bs) {
  for (int i = get_global_id(0); i < 16; i += get_global_size(0)) {
    double re = z[2 * i], im = z[2 * i + 1], b = bs[0];
    double logr = log(hypot(re, im));
    double logi = atan2(im, re);
    double x = exp(logr * b);
    double y = logi * b;
    double cosy;
    double siny = sincos(y, &cosy);
    o[2 * i] = x * cosy;
    o[2 * i + 1] = x * siny;
  }
}
