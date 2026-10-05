// Bug 4: libclc's double acospi on double16, as wasm-env compiles it: a loop
// over the 16 lanes (one copy of acospi), through private arrays.
#pragma OPENCL EXTENSION cl_khr_fp64 : enable
__kernel void v(__global double16 *out, __global const double16 *in) {
    size_t i = get_global_id(0);
    out[i] = acospi(in[i]);
}
