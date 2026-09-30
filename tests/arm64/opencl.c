// OpenCL through the Khronos loader and wasm-env's driver (packages/opencl)
// on the runtime's GPU service: a platform and device, buffers, programs
// and kernels, checked against answers computed here on the CPU (qemu has
// no GPU to compare with).
#define CL_TARGET_OPENCL_VERSION 120
#include <CL/cl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void check(cl_int e, const char *what) {
    if (e != CL_SUCCESS) {
        printf("FAILED: %s (%d)\n", what, e);
        exit(1);
    }
}

static const char *source =
    "__kernel void add(__global const float *a, __global const float *b, __global float *c, float k) {\n"
    "    int i = get_global_id(0);\n"
    "    c[i] = a[i] + k * b[i];\n"
    "}\n"
    "__kernel void reduce(__global const int *in, __global int *out, __local int *tmp) {\n"
    "    int l = get_local_id(0);\n"
    "    tmp[l] = in[get_global_id(0)];\n"
    "    barrier(CLK_LOCAL_MEM_FENCE);\n"
    "    for (int s = get_local_size(0) / 2; s > 0; s >>= 1) {\n"
    "        if (l < s) tmp[l] += tmp[l + s];\n"
    "        barrier(CLK_LOCAL_MEM_FENCE);\n"
    "    }\n"
    "    if (l == 0) atomic_add(out, tmp[0]);\n"
    "}\n"
    "typedef struct { int scale; uchar bias; } params;\n"
    "__kernel void bytes(__global uchar *p, params q) {\n"
    "    size_t i = get_global_id(0) + get_global_size(0) * get_global_id(1);\n"
    "    p[i] = p[i] * q.scale + q.bias;\n"
    "}\n";

int main(void) {
    // wasm-env's platform, among whatever others the environment has (pocl).
    cl_platform_id platforms[8], platform = NULL;
    cl_uint n;
    check(clGetPlatformIDs(8, platforms, &n), "clGetPlatformIDs");
    char name[256];
    for (cl_uint i = 0; i < n && i < 8 && !platform; i++) {
        check(clGetPlatformInfo(platforms[i], CL_PLATFORM_NAME, sizeof name, name, NULL), "clGetPlatformInfo");
        if (!strcmp(name, "wasm-env"))
            platform = platforms[i];
    }
    if (!platform)
        check(CL_INVALID_PLATFORM, "the wasm-env platform");
    printf("platform: %s\n", name);
    cl_device_id dev;
    check(clGetDeviceIDs(platform, CL_DEVICE_TYPE_ALL, 1, &dev, NULL), "clGetDeviceIDs");
    cl_int e;
    cl_context ctx = clCreateContext(NULL, 1, &dev, NULL, NULL, &e);
    check(e, "clCreateContext");
    cl_command_queue q = clCreateCommandQueue(ctx, dev, 0, &e);
    check(e, "clCreateCommandQueue");

    cl_program prog = clCreateProgramWithSource(ctx, 1, &source, NULL, &e);
    check(e, "clCreateProgramWithSource");
    check(clBuildProgram(prog, 1, &dev, "", NULL, NULL), "clBuildProgram");
    size_t count;
    check(clGetProgramInfo(prog, CL_PROGRAM_NUM_KERNELS, sizeof count, &count, NULL), "CL_PROGRAM_NUM_KERNELS");
    printf("kernels: %zu\n", count);

    // A vector sum with a scalar argument.
    enum { N = 100000 };
    float *a = malloc(N * sizeof *a), *b = malloc(N * sizeof *b), *c = malloc(N * sizeof *c);
    for (int i = 0; i < N; i++)
        a[i] = i * 0.25f, b[i] = 1.0f - i;
    cl_mem ba = clCreateBuffer(ctx, CL_MEM_READ_ONLY | CL_MEM_COPY_HOST_PTR, N * sizeof *a, a, &e);
    check(e, "clCreateBuffer");
    cl_mem bb = clCreateBuffer(ctx, CL_MEM_READ_ONLY | CL_MEM_COPY_HOST_PTR, N * sizeof *b, b, &e);
    check(e, "clCreateBuffer");
    cl_mem bc = clCreateBuffer(ctx, CL_MEM_WRITE_ONLY, N * sizeof *c, NULL, &e);
    check(e, "clCreateBuffer");
    cl_kernel add = clCreateKernel(prog, "add", &e);
    check(e, "clCreateKernel(add)");
    float k = 3.0f;
    check(clSetKernelArg(add, 0, sizeof ba, &ba) | clSetKernelArg(add, 1, sizeof bb, &bb) | clSetKernelArg(add, 2, sizeof bc, &bc) |
              clSetKernelArg(add, 3, sizeof k, &k),
          "clSetKernelArg(add)");
    size_t global = N;
    check(clEnqueueNDRangeKernel(q, add, 1, NULL, &global, NULL, 0, NULL, NULL), "clEnqueueNDRangeKernel(add)");
    check(clEnqueueReadBuffer(q, bc, CL_TRUE, 0, N * sizeof *c, c, 0, NULL, NULL), "clEnqueueReadBuffer");
    int wrong = 0;
    for (int i = 0; i < N; i++)
        wrong += c[i] != a[i] + k * b[i];
    printf("add: %d of %d wrong\n", wrong, N);

    // A reduction in local memory, with barriers and an atomic.
    enum { M = 1 << 16 };
    int *in = malloc(M * sizeof *in), expected = 0, sum = 0;
    for (int i = 0; i < M; i++)
        in[i] = (i * 7919) % 101 - 50, expected += in[i];
    cl_mem bi = clCreateBuffer(ctx, CL_MEM_COPY_HOST_PTR, M * sizeof *in, in, &e);
    check(e, "clCreateBuffer");
    cl_mem bo = clCreateBuffer(ctx, CL_MEM_READ_WRITE, sizeof sum, NULL, &e);
    check(e, "clCreateBuffer");
    check(clEnqueueFillBuffer(q, bo, &sum, sizeof sum, 0, sizeof sum, 0, NULL, NULL), "clEnqueueFillBuffer");
    cl_kernel reduce = clCreateKernel(prog, "reduce", &e);
    check(e, "clCreateKernel(reduce)");
    size_t local = 128;
    check(clSetKernelArg(reduce, 0, sizeof bi, &bi) | clSetKernelArg(reduce, 1, sizeof bo, &bo) | clSetKernelArg(reduce, 2, local * sizeof(int), NULL),
          "clSetKernelArg(reduce)");
    global = M;
    check(clEnqueueNDRangeKernel(q, reduce, 1, NULL, &global, &local, 0, NULL, NULL), "clEnqueueNDRangeKernel(reduce)");
    check(clEnqueueReadBuffer(q, bo, CL_TRUE, 0, sizeof sum, &sum, 0, NULL, NULL), "clEnqueueReadBuffer");
    printf("reduce: %s\n", sum == expected ? "right" : "wrong");

    // Bytes that neighbouring work-items write, in two dimensions, with a
    // struct argument; through a mapping.
    enum { W = 64, H = 50 };
    struct {
        cl_int scale;
        cl_uchar bias;
    } params = {3, 7};
    cl_mem bp = clCreateBuffer(ctx, CL_MEM_READ_WRITE, W * H, NULL, &e);
    check(e, "clCreateBuffer");
    unsigned char *map = clEnqueueMapBuffer(q, bp, CL_TRUE, CL_MAP_WRITE, 0, W * H, 0, NULL, NULL, &e);
    check(e, "clEnqueueMapBuffer");
    for (int i = 0; i < W * H; i++)
        map[i] = i % 251;
    check(clEnqueueUnmapMemObject(q, bp, map, 0, NULL, NULL), "clEnqueueUnmapMemObject");
    cl_kernel bytes = clCreateKernel(prog, "bytes", &e);
    check(e, "clCreateKernel(bytes)");
    check(clSetKernelArg(bytes, 0, sizeof bp, &bp) | clSetKernelArg(bytes, 1, sizeof params, &params), "clSetKernelArg(bytes)");
    size_t global2[2] = {W, H};
    check(clEnqueueNDRangeKernel(q, bytes, 2, NULL, global2, NULL, 0, NULL, NULL), "clEnqueueNDRangeKernel(bytes)");
    unsigned char out[W * H];
    check(clEnqueueReadBuffer(q, bp, CL_TRUE, 0, sizeof out, out, 0, NULL, NULL), "clEnqueueReadBuffer");
    wrong = 0;
    for (int i = 0; i < W * H; i++)
        wrong += out[i] != (unsigned char)((i % 251) * 3 + 7);
    printf("bytes: %d of %d wrong\n", wrong, W * H);

    // A program that doesn't compile: the build fails with a log.
    const char *bad = "__kernel void k(__global int *p) { p[0] = undefined_thing; }";
    cl_program broken = clCreateProgramWithSource(ctx, 1, &bad, NULL, &e);
    check(e, "clCreateProgramWithSource");
    e = clBuildProgram(broken, 1, &dev, "", NULL, NULL);
    char log[4096] = "";
    clGetProgramBuildInfo(broken, dev, CL_PROGRAM_BUILD_LOG, sizeof log, log, NULL);
    printf("bad program: %s, log mentions it: %s\n", e == CL_BUILD_PROGRAM_FAILURE ? "fails" : "builds?", strstr(log, "undefined_thing") ? "yes" : "no");

    clReleaseKernel(add), clReleaseKernel(reduce), clReleaseKernel(bytes);
    clReleaseProgram(prog), clReleaseProgram(broken);
    clReleaseMemObject(ba), clReleaseMemObject(bb), clReleaseMemObject(bc), clReleaseMemObject(bi), clReleaseMemObject(bo), clReleaseMemObject(bp);
    clReleaseCommandQueue(q);
    clReleaseContext(ctx);
    puts("done");
    return 0;
}
