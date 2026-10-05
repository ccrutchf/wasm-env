// Bug 3: three double sins in a branch (libclc's sin, software doubles).
// Each work-item but the last computes sin of three inputs.
#pragma OPENCL EXTENSION cl_khr_fp64 : enable
__kernel void k(__global const double *in, __global double *out) {
    size_t i = get_global_id(0);
    if (i + 1 < get_global_size(0)) {
        out[3 * i] = sin(in[3 * i]);
        out[3 * i + 1] = sin(in[3 * i + 1]);
        out[3 * i + 2] = sin(in[3 * i + 2]);
    }
}
