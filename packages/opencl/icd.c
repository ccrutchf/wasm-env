// wasm-env's OpenCL driver: an installable client driver (ICD) for the
// Khronos loader (Ubuntu's ocl-icd), implementing OpenCL 1.2 on the
// runtime's GPU service through /dev/wasm-env-gpu (runtime/src/gpu.rs has
// the requests). One platform, one device. Commands run in order as
// they're enqueued, so events are complete when made; reads wait for the
// work before them.
//
// It uses nothing from the C library but allocation, a few string
// functions, open, ioctl and close, resolved when it's loaded: the same
// library serves glibc and musl programs.
#define CL_TARGET_OPENCL_VERSION 300
#define CL_USE_DEPRECATED_OPENCL_1_2_APIS
#include <CL/cl.h>
#include <CL/cl_ext.h>
#include <CL/cl_icd.h>
#include <stddef.h>
#include <stdint.h>

// The C library's functions it uses, declared here rather than from a C
// library's headers: any compiler for aarch64 builds it (the toolchain's
// clang, on any host), and the program's own C library provides them.
void *malloc(size_t);
void *calloc(size_t, size_t);
void free(void *);
void *memcpy(void *, const void *, size_t);
void *memset(void *, int, size_t);
size_t strlen(const char *);
int strcmp(const char *, const char *);
char *strchr(const char *, int);
int open(const char *, int, ...);
int ioctl(int, unsigned long, ...);
int close(int);
#define O_RDWR 02
#define O_CLOEXEC 02000000

#define API __attribute__((visibility("default")))
#define GPU_CALL 0xc0805740u

enum { INFO = 1, CREATE, FREE, WRITE, READ, COPY, FILL, BUILD, LOG, KERNELS, ARGS, RUN, FINISH };

struct request {
    uint64_t op, a[9], r[6];
};

static struct _cl_icd_dispatch dispatch;

struct _cl_platform_id {
    struct _cl_icd_dispatch *d;
};
struct _cl_device_id {
    struct _cl_icd_dispatch *d;
};
struct _cl_context {
    struct _cl_icd_dispatch *d;
    cl_uint refs;
    cl_context_properties props[8];
    size_t nprops;
};
struct _cl_command_queue {
    struct _cl_icd_dispatch *d;
    cl_uint refs;
    cl_context ctx;
    cl_command_queue_properties props;
};
struct _cl_mem {
    struct _cl_icd_dispatch *d;
    cl_uint refs;
    cl_context ctx;
    cl_mem_flags flags;
    size_t size, offset;
    uint64_t handle;
    cl_mem parent;
    void *host;
    cl_uint maps;
};
struct _cl_program {
    struct _cl_icd_dispatch *d;
    cl_uint refs;
    cl_context ctx;
    char *source, *options;
    size_t len;
    uint64_t handle;
    cl_build_status status;
    cl_uint kernels;
};
struct arg {
    uint32_t kind, size, offset, zero;
    char type[56], name[56];
};
struct _cl_kernel {
    struct _cl_icd_dispatch *d;
    cl_uint refs;
    cl_program program;
    char *name;
    cl_uint nargs;
    struct arg *args;
    uint8_t *block;
    size_t block_size;
    uint64_t *handles;
    uint32_t *locals;
    uint8_t *set;
    size_t required[3];
};
struct _cl_event {
    struct _cl_icd_dispatch *d;
    cl_uint refs;
    cl_context ctx;
    cl_command_queue queue;
    cl_command_type type;
    cl_int status;
};

static struct _cl_platform_id platform = {&dispatch};
static struct _cl_device_id device = {&dispatch};
static int gpu = -1;

// The device's description (INFO).
static struct {
    char name[64], vendor[64], driver[64];
    uint32_t type, max_group, max_items[3], units;
    uint64_t local_mem, max_alloc, global_mem;
    uint32_t max_buffers, vendor_id;
    uint8_t pad[8];
} desc;

static int call(struct request *r) {
    return ioctl(gpu, GPU_CALL, r) == 0 ? (int)(int64_t)r->r[1] : CL_OUT_OF_RESOURCES;
}

static int open_gpu(void) {
    if (gpu >= 0)
        return 1;
    int fd = open("/dev/wasm-env-gpu", O_RDWR | O_CLOEXEC);
    if (fd < 0)
        return 0;
    gpu = fd;
    struct request r = {INFO, {(uintptr_t)&desc, sizeof desc}};
    if (call(&r) != CL_SUCCESS) {
        close(fd);
        gpu = -1;
        return 0;
    }
    return 1;
}

// clGet*Info's contract: the value if there's room, its size.
static cl_int info(const void *v, size_t size, size_t cap, void *out, size_t *size_ret) {
    if (out) {
        if (cap < size)
            return CL_INVALID_VALUE;
        memcpy(out, v, size);
    }
    if (size_ret)
        *size_ret = size;
    return CL_SUCCESS;
}
#define INFO_OF(type, value)                                                                                                                                   \
    do {                                                                                                                                                       \
        type v_ = (value);                                                                                                                                     \
        return info(&v_, sizeof v_, cap, out, size_ret);                                                                                                       \
    } while (0)
#define INFO_STR(s) return info((s), strlen(s) + 1, cap, out, size_ret)

static char *dup_bytes(const char *s, size_t n) {
    char *p = malloc(n + 1);
    if (p) {
        memcpy(p, s, n);
        p[n] = 0;
    }
    return p;
}

static cl_event event(cl_command_queue q, cl_command_type type) {
    cl_event e = calloc(1, sizeof *e);
    if (e) {
        *e = (struct _cl_event){&dispatch, 1, q ? q->ctx : NULL, q, type, CL_COMPLETE};
        if (q)
            q->refs++;
    }
    return e;
}

static cl_int done(cl_command_queue q, cl_command_type type, cl_event *ev) {
    if (ev && !(*ev = event(q, type)))
        return CL_OUT_OF_HOST_MEMORY;
    return CL_SUCCESS;
}

// ---- Platform and device ----

API cl_int clIcdGetPlatformIDsKHR(cl_uint n, cl_platform_id *platforms, cl_uint *count) {
    if ((!platforms && !count) || (platforms && !n))
        return CL_INVALID_VALUE;
    int ok = open_gpu();
    if (count)
        *count = ok;
    if (platforms && ok)
        platforms[0] = &platform;
    return ok ? CL_SUCCESS : CL_PLATFORM_NOT_FOUND_KHR;
}

static cl_int GetPlatformInfo(cl_platform_id p, cl_platform_info what, size_t cap, void *out, size_t *size_ret) {
    if (p && p != &platform)
        return CL_INVALID_PLATFORM;
    switch (what) {
    case CL_PLATFORM_PROFILE:
        INFO_STR("FULL_PROFILE");
    case CL_PLATFORM_VERSION:
        INFO_STR("OpenCL 1.2 wasm-env");
    case CL_PLATFORM_NAME:
        INFO_STR("wasm-env");
    case CL_PLATFORM_VENDOR:
        INFO_STR("wasm-env");
    case CL_PLATFORM_EXTENSIONS:
        INFO_STR("cl_khr_icd");
    case CL_PLATFORM_ICD_SUFFIX_KHR:
        INFO_STR("WENV");
    case CL_PLATFORM_HOST_TIMER_RESOLUTION:
        INFO_OF(cl_ulong, 0);
    }
    return CL_INVALID_VALUE;
}

static cl_int GetDeviceIDs(cl_platform_id p, cl_device_type type, cl_uint n, cl_device_id *devices, cl_uint *count) {
    if (p && p != &platform)
        return CL_INVALID_PLATFORM;
    if ((!devices && !count) || (devices && !n))
        return CL_INVALID_VALUE;
    int match = type == CL_DEVICE_TYPE_ALL || type == CL_DEVICE_TYPE_DEFAULT || (type & desc.type);
    if (count)
        *count = match;
    if (!match)
        return CL_DEVICE_NOT_FOUND;
    if (devices)
        devices[0] = &device;
    return CL_SUCCESS;
}

static const char extensions[] = "cl_khr_global_int32_base_atomics cl_khr_global_int32_extended_atomics cl_khr_local_int32_base_atomics "
                                 "cl_khr_local_int32_extended_atomics cl_khr_byte_addressable_store";

static cl_int GetDeviceInfo(cl_device_id dev, cl_device_info what, size_t cap, void *out, size_t *size_ret) {
    if (dev != &device)
        return CL_INVALID_DEVICE;
    switch (what) {
    case CL_DEVICE_TYPE:
        INFO_OF(cl_device_type, desc.type);
    case CL_DEVICE_VENDOR_ID:
        INFO_OF(cl_uint, desc.vendor_id);
    case CL_DEVICE_MAX_COMPUTE_UNITS:
        INFO_OF(cl_uint, desc.units);
    case CL_DEVICE_MAX_WORK_ITEM_DIMENSIONS:
        INFO_OF(cl_uint, 3);
    case CL_DEVICE_MAX_WORK_ITEM_SIZES: {
        size_t s[3] = {desc.max_items[0], desc.max_items[1], desc.max_items[2]};
        return info(s, sizeof s, cap, out, size_ret);
    }
    case CL_DEVICE_MAX_WORK_GROUP_SIZE:
        INFO_OF(size_t, desc.max_group);
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_CHAR:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_CHAR:
        INFO_OF(cl_uint, 4);
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_SHORT:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_SHORT:
        INFO_OF(cl_uint, 2);
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_INT:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_INT:
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_LONG:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_LONG:
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_FLOAT:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_FLOAT:
        INFO_OF(cl_uint, 1);
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_DOUBLE:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_DOUBLE:
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_HALF:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_HALF:
        INFO_OF(cl_uint, 0);
    case CL_DEVICE_MAX_CLOCK_FREQUENCY:
        INFO_OF(cl_uint, 1000);
    case CL_DEVICE_ADDRESS_BITS:
        INFO_OF(cl_uint, 32);
    case CL_DEVICE_MAX_MEM_ALLOC_SIZE:
    case CL_DEVICE_MAX_CONSTANT_BUFFER_SIZE:
        INFO_OF(cl_ulong, desc.max_alloc);
    case CL_DEVICE_GLOBAL_MEM_SIZE:
        INFO_OF(cl_ulong, desc.global_mem);
    case CL_DEVICE_IMAGE_SUPPORT:
    case CL_DEVICE_ERROR_CORRECTION_SUPPORT:
    case CL_DEVICE_HOST_UNIFIED_MEMORY:
        INFO_OF(cl_bool, CL_FALSE);
    case CL_DEVICE_ENDIAN_LITTLE:
    case CL_DEVICE_AVAILABLE:
    case CL_DEVICE_COMPILER_AVAILABLE:
    case CL_DEVICE_LINKER_AVAILABLE:
    case CL_DEVICE_PREFERRED_INTEROP_USER_SYNC:
        INFO_OF(cl_bool, CL_TRUE);
    case CL_DEVICE_MAX_READ_IMAGE_ARGS:
    case CL_DEVICE_MAX_WRITE_IMAGE_ARGS:
    case CL_DEVICE_MAX_SAMPLERS:
    case CL_DEVICE_GLOBAL_MEM_CACHELINE_SIZE:
    case CL_DEVICE_PARTITION_MAX_SUB_DEVICES:
    case CL_DEVICE_REFERENCE_COUNT - 0:
        INFO_OF(cl_uint, what == CL_DEVICE_REFERENCE_COUNT ? 1 : 0);
    case CL_DEVICE_IMAGE2D_MAX_WIDTH:
    case CL_DEVICE_IMAGE2D_MAX_HEIGHT:
    case CL_DEVICE_IMAGE3D_MAX_WIDTH:
    case CL_DEVICE_IMAGE3D_MAX_HEIGHT:
    case CL_DEVICE_IMAGE3D_MAX_DEPTH:
    case CL_DEVICE_IMAGE_MAX_BUFFER_SIZE:
    case CL_DEVICE_IMAGE_MAX_ARRAY_SIZE:
    case CL_DEVICE_PRINTF_BUFFER_SIZE:
        INFO_OF(size_t, 0);
    case CL_DEVICE_MAX_PARAMETER_SIZE:
        INFO_OF(size_t, 1024);
    case CL_DEVICE_MEM_BASE_ADDR_ALIGN:
        INFO_OF(cl_uint, 1024);
    case CL_DEVICE_MIN_DATA_TYPE_ALIGN_SIZE:
        INFO_OF(cl_uint, 128);
    case CL_DEVICE_SINGLE_FP_CONFIG:
        INFO_OF(cl_device_fp_config, CL_FP_ROUND_TO_NEAREST | CL_FP_INF_NAN);
    case CL_DEVICE_DOUBLE_FP_CONFIG:
    case CL_DEVICE_HALF_FP_CONFIG:
        INFO_OF(cl_device_fp_config, 0);
    case CL_DEVICE_GLOBAL_MEM_CACHE_TYPE:
        INFO_OF(cl_device_mem_cache_type, CL_NONE);
    case CL_DEVICE_GLOBAL_MEM_CACHE_SIZE:
        INFO_OF(cl_ulong, 0);
    case CL_DEVICE_MAX_CONSTANT_ARGS:
        INFO_OF(cl_uint, desc.max_buffers < 8 ? desc.max_buffers : 8);
    case CL_DEVICE_LOCAL_MEM_TYPE:
        INFO_OF(cl_device_local_mem_type, CL_LOCAL);
    case CL_DEVICE_LOCAL_MEM_SIZE:
        INFO_OF(cl_ulong, desc.local_mem);
    case CL_DEVICE_PROFILING_TIMER_RESOLUTION:
        INFO_OF(size_t, 1);
    case CL_DEVICE_EXECUTION_CAPABILITIES:
        INFO_OF(cl_device_exec_capabilities, CL_EXEC_KERNEL);
    case CL_DEVICE_QUEUE_PROPERTIES:
        INFO_OF(cl_command_queue_properties, CL_QUEUE_PROFILING_ENABLE);
    case CL_DEVICE_NAME:
        INFO_STR(desc.name);
    case CL_DEVICE_VENDOR:
        INFO_STR(desc.vendor);
    case CL_DRIVER_VERSION:
        INFO_STR(desc.driver);
    case CL_DEVICE_PROFILE:
        INFO_STR("FULL_PROFILE");
    case CL_DEVICE_VERSION:
        INFO_STR("OpenCL 1.2 wasm-env");
    case CL_DEVICE_OPENCL_C_VERSION:
        INFO_STR("OpenCL C 1.2 ");
    case CL_DEVICE_EXTENSIONS:
        INFO_STR(extensions);
    case CL_DEVICE_BUILT_IN_KERNELS:
        INFO_STR("");
    case CL_DEVICE_PLATFORM:
        INFO_OF(cl_platform_id, &platform);
    case CL_DEVICE_PARENT_DEVICE:
        INFO_OF(cl_device_id, NULL);
    case CL_DEVICE_PARTITION_PROPERTIES:
    case CL_DEVICE_PARTITION_TYPE:
        INFO_OF(cl_device_partition_property, 0);
    case CL_DEVICE_PARTITION_AFFINITY_DOMAIN:
        INFO_OF(cl_device_affinity_domain, 0);
    }
    return CL_INVALID_VALUE;
}

static cl_int RetainDevice(cl_device_id d) { return d == &device ? CL_SUCCESS : CL_INVALID_DEVICE; }
static cl_int ReleaseDevice(cl_device_id d) { return d == &device ? CL_SUCCESS : CL_INVALID_DEVICE; }

// ---- Contexts and queues ----

static cl_context CreateContext(const cl_context_properties *props, cl_uint n, const cl_device_id *devices,
                                void(CL_CALLBACK *notify)(const char *, const void *, size_t, void *), void *user, cl_int *err) {
    (void)notify, (void)user;
    cl_int e = CL_SUCCESS;
    if (!n || !devices)
        e = CL_INVALID_VALUE;
    for (cl_uint i = 0; e == CL_SUCCESS && i < n; i++)
        if (devices[i] != &device)
            e = CL_INVALID_DEVICE;
    cl_context c = e == CL_SUCCESS ? calloc(1, sizeof *c) : NULL;
    if (e == CL_SUCCESS && !c)
        e = CL_OUT_OF_HOST_MEMORY;
    if (c) {
        c->d = &dispatch;
        c->refs = 1;
        for (size_t i = 0; props && props[i] && i + 2 < 8; i += 2) {
            c->props[i] = props[i];
            c->props[i + 1] = props[i + 1];
            c->nprops = i + 3;
        }
    }
    if (err)
        *err = e;
    return c;
}

static cl_context CreateContextFromType(const cl_context_properties *props, cl_device_type type,
                                        void(CL_CALLBACK *notify)(const char *, const void *, size_t, void *), void *user, cl_int *err) {
    if (!(type == CL_DEVICE_TYPE_ALL || type == CL_DEVICE_TYPE_DEFAULT || (type & desc.type))) {
        if (err)
            *err = CL_DEVICE_NOT_FOUND;
        return NULL;
    }
    cl_device_id d = &device;
    return CreateContext(props, 1, &d, notify, user, err);
}

static cl_int RetainContext(cl_context c) {
    if (!c)
        return CL_INVALID_CONTEXT;
    c->refs++;
    return CL_SUCCESS;
}

static cl_int ReleaseContext(cl_context c) {
    if (!c)
        return CL_INVALID_CONTEXT;
    if (--c->refs == 0)
        free(c);
    return CL_SUCCESS;
}

static cl_int GetContextInfo(cl_context c, cl_context_info what, size_t cap, void *out, size_t *size_ret) {
    if (!c)
        return CL_INVALID_CONTEXT;
    switch (what) {
    case CL_CONTEXT_REFERENCE_COUNT:
        INFO_OF(cl_uint, c->refs);
    case CL_CONTEXT_NUM_DEVICES:
        INFO_OF(cl_uint, 1);
    case CL_CONTEXT_DEVICES:
        INFO_OF(cl_device_id, &device);
    case CL_CONTEXT_PROPERTIES:
        return info(c->props, c->nprops * sizeof(cl_context_properties), cap, out, size_ret);
    }
    return CL_INVALID_VALUE;
}

static cl_command_queue queue(cl_context c, cl_device_id d, cl_command_queue_properties props, cl_int *err) {
    cl_int e = !c ? CL_INVALID_CONTEXT : d != &device ? CL_INVALID_DEVICE : CL_SUCCESS;
    cl_command_queue q = e == CL_SUCCESS ? calloc(1, sizeof *q) : NULL;
    if (q) {
        *q = (struct _cl_command_queue){&dispatch, 1, c, props};
        c->refs++;
    } else if (e == CL_SUCCESS) {
        e = CL_OUT_OF_HOST_MEMORY;
    }
    if (err)
        *err = e;
    return q;
}

static cl_command_queue CreateCommandQueue(cl_context c, cl_device_id d, cl_command_queue_properties props, cl_int *err) {
    return queue(c, d, props, err);
}

static cl_command_queue CreateCommandQueueWithProperties(cl_context c, cl_device_id d, const cl_queue_properties *props, cl_int *err) {
    cl_command_queue_properties p = 0;
    for (size_t i = 0; props && props[i]; i += 2)
        if (props[i] == CL_QUEUE_PROPERTIES)
            p = props[i + 1];
    return queue(c, d, p, err);
}

static cl_int RetainCommandQueue(cl_command_queue q) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    q->refs++;
    return CL_SUCCESS;
}

static cl_int ReleaseCommandQueue(cl_command_queue q) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (--q->refs == 0) {
        ReleaseContext(q->ctx);
        free(q);
    }
    return CL_SUCCESS;
}

static cl_int GetCommandQueueInfo(cl_command_queue q, cl_command_queue_info what, size_t cap, void *out, size_t *size_ret) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    switch (what) {
    case CL_QUEUE_CONTEXT:
        INFO_OF(cl_context, q->ctx);
    case CL_QUEUE_DEVICE:
        INFO_OF(cl_device_id, &device);
    case CL_QUEUE_REFERENCE_COUNT:
        INFO_OF(cl_uint, q->refs);
    case CL_QUEUE_PROPERTIES:
        INFO_OF(cl_command_queue_properties, q->props);
    }
    return CL_INVALID_VALUE;
}

static cl_int Flush(cl_command_queue q) { return q ? CL_SUCCESS : CL_INVALID_COMMAND_QUEUE; }

static cl_int Finish(cl_command_queue q) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    struct request r = {FINISH};
    return call(&r);
}

// ---- Memory ----

static uint64_t base_handle(cl_mem m) { return m->parent ? m->parent->handle : m->handle; }
static size_t base_offset(cl_mem m) { return m->offset; }

static cl_int write_mem(cl_mem m, size_t offset, size_t size, const void *p) {
    struct request r = {WRITE, {base_handle(m), base_offset(m) + offset, (uintptr_t)p, size}};
    return call(&r);
}

static cl_int read_mem(cl_mem m, size_t offset, size_t size, void *p) {
    struct request r = {READ, {base_handle(m), base_offset(m) + offset, (uintptr_t)p, size}};
    return call(&r);
}

static cl_mem CreateBuffer(cl_context c, cl_mem_flags flags, size_t size, void *host, cl_int *err) {
    cl_int e = CL_SUCCESS;
    int wants_host = (flags & (CL_MEM_USE_HOST_PTR | CL_MEM_COPY_HOST_PTR)) != 0;
    if (!c)
        e = CL_INVALID_CONTEXT;
    else if (!size || size > desc.max_alloc)
        e = CL_INVALID_BUFFER_SIZE;
    else if (wants_host != (host != NULL))
        e = CL_INVALID_HOST_PTR;
    cl_mem m = NULL;
    if (e == CL_SUCCESS) {
        struct request r = {CREATE, {size}};
        e = call(&r);
        if (e == CL_SUCCESS && !(m = calloc(1, sizeof *m))) {
            struct request f = {FREE, {r.r[0]}};
            call(&f);
            e = CL_OUT_OF_HOST_MEMORY;
        }
        if (m) {
            *m = (struct _cl_mem){&dispatch, 1, c, flags ? flags : CL_MEM_READ_WRITE, size, 0, r.r[0]};
            m->host = flags & CL_MEM_USE_HOST_PTR ? host : NULL;
            c->refs++;
            if (host && (e = write_mem(m, 0, size, host)) != CL_SUCCESS) {
                struct request f = {FREE, {m->handle}};
                call(&f);
                free(m);
                m = NULL;
            }
        }
    }
    if (err)
        *err = e;
    return m;
}

static cl_mem CreateSubBuffer(cl_mem parent, cl_mem_flags flags, cl_buffer_create_type type, const void *region_info, cl_int *err) {
    const cl_buffer_region *region = region_info;
    cl_int e = CL_SUCCESS;
    if (!parent || parent->parent)
        e = CL_INVALID_MEM_OBJECT;
    else if (type != CL_BUFFER_CREATE_TYPE_REGION || !region || region->origin + region->size > parent->size || !region->size)
        e = CL_INVALID_VALUE;
    cl_mem m = e == CL_SUCCESS ? calloc(1, sizeof *m) : NULL;
    if (m) {
        *m = (struct _cl_mem){&dispatch, 1, parent->ctx, flags ? flags : parent->flags, region->size, region->origin, 0, parent};
        parent->refs++;
        parent->ctx->refs++;
    } else if (e == CL_SUCCESS) {
        e = CL_OUT_OF_HOST_MEMORY;
    }
    if (err)
        *err = e;
    return m;
}

static cl_int RetainMemObject(cl_mem m) {
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    m->refs++;
    return CL_SUCCESS;
}

static cl_int ReleaseMemObject(cl_mem m) {
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (--m->refs == 0) {
        if (m->parent) {
            ReleaseMemObject(m->parent);
        } else {
            struct request r = {FREE, {m->handle}};
            call(&r);
        }
        ReleaseContext(m->ctx);
        free(m);
    }
    return CL_SUCCESS;
}

static cl_int GetMemObjectInfo(cl_mem m, cl_mem_info what, size_t cap, void *out, size_t *size_ret) {
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    switch (what) {
    case CL_MEM_TYPE:
        INFO_OF(cl_mem_object_type, CL_MEM_OBJECT_BUFFER);
    case CL_MEM_FLAGS:
        INFO_OF(cl_mem_flags, m->flags);
    case CL_MEM_SIZE:
        INFO_OF(size_t, m->size);
    case CL_MEM_HOST_PTR:
        INFO_OF(void *, m->host);
    case CL_MEM_MAP_COUNT:
        INFO_OF(cl_uint, m->maps);
    case CL_MEM_REFERENCE_COUNT:
        INFO_OF(cl_uint, m->refs);
    case CL_MEM_CONTEXT:
        INFO_OF(cl_context, m->ctx);
    case CL_MEM_ASSOCIATED_MEMOBJECT:
        INFO_OF(cl_mem, m->parent);
    case CL_MEM_OFFSET:
        INFO_OF(size_t, m->offset);
    }
    return CL_INVALID_VALUE;
}

static cl_int GetSupportedImageFormats(cl_context c, cl_mem_flags flags, cl_mem_object_type type, cl_uint n, cl_image_format *formats, cl_uint *count) {
    (void)flags, (void)type, (void)n, (void)formats;
    if (!c)
        return CL_INVALID_CONTEXT;
    if (count)
        *count = 0;
    return CL_SUCCESS;
}

static cl_int EnqueueReadBuffer(cl_command_queue q, cl_mem m, cl_bool blocking, size_t offset, size_t size, void *p, cl_uint nw, const cl_event *wait,
                                cl_event *ev) {
    (void)blocking, (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (!p || offset + size > m->size)
        return CL_INVALID_VALUE;
    cl_int e = read_mem(m, offset, size, p);
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_READ_BUFFER, ev);
}

static cl_int EnqueueWriteBuffer(cl_command_queue q, cl_mem m, cl_bool blocking, size_t offset, size_t size, const void *p, cl_uint nw,
                                 const cl_event *wait, cl_event *ev) {
    (void)blocking, (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (!p || offset + size > m->size)
        return CL_INVALID_VALUE;
    cl_int e = write_mem(m, offset, size, p);
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_WRITE_BUFFER, ev);
}

static cl_int EnqueueCopyBuffer(cl_command_queue q, cl_mem src, cl_mem dst, size_t so, size_t dof, size_t size, cl_uint nw, const cl_event *wait,
                                cl_event *ev) {
    (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!src || !dst)
        return CL_INVALID_MEM_OBJECT;
    if (so + size > src->size || dof + size > dst->size)
        return CL_INVALID_VALUE;
    struct request r = {COPY, {base_handle(src), base_offset(src) + so, base_handle(dst), base_offset(dst) + dof, size}};
    cl_int e = call(&r);
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_COPY_BUFFER, ev);
}

static cl_int EnqueueFillBuffer(cl_command_queue q, cl_mem m, const void *pattern, size_t psize, size_t offset, size_t size, cl_uint nw,
                                const cl_event *wait, cl_event *ev) {
    (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (!pattern || !psize || offset + size > m->size || size % psize || offset % psize)
        return CL_INVALID_VALUE;
    struct request r = {FILL, {base_handle(m), base_offset(m) + offset, (uintptr_t)pattern, psize, size}};
    cl_int e = call(&r);
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_FILL_BUFFER, ev);
}

// Rectangles: row by row.
static cl_int rect(cl_mem m, int writing, const size_t *bo, const size_t *ho, const size_t *reg, size_t brp, size_t bsp, size_t hrp, size_t hsp,
                   void *p) {
    if (!brp)
        brp = reg[0];
    if (!bsp)
        bsp = reg[1] * brp;
    if (!hrp)
        hrp = reg[0];
    if (!hsp)
        hsp = reg[1] * hrp;
    for (size_t z = 0; z < reg[2]; z++)
        for (size_t y = 0; y < reg[1]; y++) {
            size_t b = (bo[2] + z) * bsp + (bo[1] + y) * brp + bo[0];
            char *h = (char *)p + (ho[2] + z) * hsp + (ho[1] + y) * hrp + ho[0];
            if (b + reg[0] > m->size)
                return CL_INVALID_VALUE;
            cl_int e = writing ? write_mem(m, b, reg[0], h) : read_mem(m, b, reg[0], h);
            if (e != CL_SUCCESS)
                return e;
        }
    return CL_SUCCESS;
}

static cl_int EnqueueReadBufferRect(cl_command_queue q, cl_mem m, cl_bool blocking, const size_t *bo, const size_t *ho, const size_t *reg, size_t brp,
                                    size_t bsp, size_t hrp, size_t hsp, void *p, cl_uint nw, const cl_event *wait, cl_event *ev) {
    (void)blocking, (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    cl_int e = rect(m, 0, bo, ho, reg, brp, bsp, hrp, hsp, p);
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_READ_BUFFER_RECT, ev);
}

static cl_int EnqueueWriteBufferRect(cl_command_queue q, cl_mem m, cl_bool blocking, const size_t *bo, const size_t *ho, const size_t *reg, size_t brp,
                                     size_t bsp, size_t hrp, size_t hsp, const void *p, cl_uint nw, const cl_event *wait, cl_event *ev) {
    (void)blocking, (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    cl_int e = rect(m, 1, bo, ho, reg, brp, bsp, hrp, hsp, (void *)p);
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_WRITE_BUFFER_RECT, ev);
}


// Mapped regions of buffers not made from the host's memory: copies,
// written back at unmap.
struct mapping {
    void *p;
    size_t offset, size;
    cl_mem m;
    int write;
    struct mapping *next;
};
static struct mapping *mappings;

static void *EnqueueMapBuffer(cl_command_queue q, cl_mem m, cl_bool blocking, cl_map_flags flags, size_t offset, size_t size, cl_uint nw,
                              const cl_event *wait, cl_event *ev, cl_int *err) {
    (void)blocking, (void)nw, (void)wait;
    cl_int e = !q ? CL_INVALID_COMMAND_QUEUE : !m ? CL_INVALID_MEM_OBJECT : offset + size > m->size ? CL_INVALID_VALUE : CL_SUCCESS;
    void *p = NULL;
    struct mapping *map = NULL;
    if (e == CL_SUCCESS) {
        // A buffer made from the host's memory maps to it.
        p = m->host ? (char *)m->host + offset : malloc(size ? size : 1);
        map = m->host ? NULL : malloc(sizeof *map);
        if (!p || (!m->host && !map))
            e = CL_MAP_FAILURE;
        else if (!(flags & CL_MAP_WRITE_INVALIDATE_REGION))
            e = read_mem(m, offset, size, p);
    }
    if (e == CL_SUCCESS) {
        m->maps++;
        int write = (flags & (CL_MAP_WRITE | CL_MAP_WRITE_INVALIDATE_REGION)) != 0;
        if (map) {
            *map = (struct mapping){p, offset, size, m, write, mappings};
            mappings = map;
        } else if (write) {
            // (Written back from the host memory at unmap too.)
            struct mapping *h = malloc(sizeof *h);
            if (h) {
                *h = (struct mapping){p, offset, size, m, write, mappings};
                mappings = h;
            }
        }
        e = done(q, CL_COMMAND_MAP_BUFFER, ev);
    } else {
        if (!m || !m->host)
            free(p);
        free(map);
    }
    if (err)
        *err = e;
    return e == CL_SUCCESS ? p : NULL;
}

static cl_int EnqueueUnmapMemObject(cl_command_queue q, cl_mem m, void *p, cl_uint nw, const cl_event *wait, cl_event *ev) {
    (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m || !m->maps)
        return CL_INVALID_MEM_OBJECT;
    cl_int e = CL_SUCCESS;
    for (struct mapping **at = &mappings; *at; at = &(*at)->next) {
        struct mapping *map = *at;
        if (map->m != m || map->p != p)
            continue;
        if (map->write)
            e = write_mem(m, map->offset, map->size, p);
        *at = map->next;
        if (!m->host)
            free(p);
        free(map);
        break;
    }
    m->maps--;
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_UNMAP_MEM_OBJECT, ev);
}

// ---- Programs ----

static cl_program program(cl_context c, char *source, size_t len, cl_int *err) {
    cl_program p = source ? calloc(1, sizeof *p) : NULL;
    if (p) {
        *p = (struct _cl_program){&dispatch, 1, c, source, NULL, len, 0, CL_BUILD_NONE};
        c->refs++;
    } else {
        free(source);
    }
    if (err)
        *err = p ? CL_SUCCESS : CL_OUT_OF_HOST_MEMORY;
    return p;
}

static cl_program CreateProgramWithSource(cl_context c, cl_uint n, const char **strings, const size_t *lengths, cl_int *err) {
    if (!c || !n || !strings) {
        if (err)
            *err = !c ? CL_INVALID_CONTEXT : CL_INVALID_VALUE;
        return NULL;
    }
    size_t len = 0;
    for (cl_uint i = 0; i < n; i++)
        len += lengths && lengths[i] ? lengths[i] : strlen(strings[i]);
    char *s = malloc(len + 1);
    size_t at = 0;
    for (cl_uint i = 0; s && i < n; i++) {
        size_t l = lengths && lengths[i] ? lengths[i] : strlen(strings[i]);
        memcpy(s + at, strings[i], l);
        at += l;
    }
    if (s)
        s[len] = 0;
    return program(c, s, len, err);
}

// A program's "binary" is its source: built again when loaded.
static cl_program CreateProgramWithBinary(cl_context c, cl_uint n, const cl_device_id *devices, const size_t *lengths, const unsigned char **binaries,
                                          cl_int *status, cl_int *err) {
    if (!c || n != 1 || !devices || devices[0] != &device || !lengths || !binaries || !binaries[0]) {
        if (err)
            *err = !c ? CL_INVALID_CONTEXT : CL_INVALID_VALUE;
        return NULL;
    }
    if (status)
        status[0] = CL_SUCCESS;
    return program(c, dup_bytes((const char *)binaries[0], lengths[0]), lengths[0], err);
}

static cl_int RetainProgram(cl_program p) {
    if (!p)
        return CL_INVALID_PROGRAM;
    p->refs++;
    return CL_SUCCESS;
}

static cl_int ReleaseProgram(cl_program p) {
    if (!p)
        return CL_INVALID_PROGRAM;
    if (--p->refs == 0) {
        if (p->handle) {
            struct request r = {FREE, {p->handle}};
            call(&r);
        }
        ReleaseContext(p->ctx);
        free(p->source);
        free(p->options);
        free(p);
    }
    return CL_SUCCESS;
}

static cl_int BuildProgram(cl_program p, cl_uint n, const cl_device_id *devices, const char *options, void(CL_CALLBACK *notify)(cl_program, void *),
                           void *user) {
    if (!p)
        return CL_INVALID_PROGRAM;
    if ((n && !devices) || (!n && devices))
        return CL_INVALID_VALUE;
    for (cl_uint i = 0; i < n; i++)
        if (devices[i] != &device)
            return CL_INVALID_DEVICE;
    if (p->handle) {
        struct request f = {FREE, {p->handle}};
        call(&f);
        p->handle = 0;
    }
    free(p->options);
    p->options = dup_bytes(options ? options : "", options ? strlen(options) : 0);
    struct request r = {BUILD, {(uintptr_t)p->source, p->len, (uintptr_t)p->options, strlen(p->options)}};
    cl_int e = call(&r);
    p->handle = r.r[0];
    p->status = e == CL_SUCCESS ? CL_BUILD_SUCCESS : CL_BUILD_ERROR;
    if (notify)
        notify(p, user);
    return e;
}

static cl_int CompileProgram(cl_program p, cl_uint n, const cl_device_id *devices, const char *options, cl_uint nh, const cl_program *headers,
                             const char **names, void(CL_CALLBACK *notify)(cl_program, void *), void *user) {
    (void)nh, (void)headers, (void)names;
    return BuildProgram(p, n, devices, options, notify, user);
}

static cl_program LinkProgram(cl_context c, cl_uint n, const cl_device_id *devices, const char *options, cl_uint np, const cl_program *programs,
                              void(CL_CALLBACK *notify)(cl_program, void *), void *user, cl_int *err) {
    const char **sources = malloc(np * sizeof *sources);
    size_t *lengths = malloc(np * sizeof *lengths);
    cl_program p = NULL;
    if (sources && lengths) {
        for (cl_uint i = 0; i < np; i++)
            sources[i] = programs[i]->source, lengths[i] = programs[i]->len;
        p = CreateProgramWithSource(c, np, sources, lengths, err);
    }
    free(sources);
    free(lengths);
    if (p) {
        cl_int e = BuildProgram(p, n, devices, options, notify, user);
        if (err)
            *err = e == CL_BUILD_PROGRAM_FAILURE ? CL_LINK_PROGRAM_FAILURE : e;
    }
    return p;
}

static cl_int UnloadPlatformCompiler(cl_platform_id p) { return p == &platform ? CL_SUCCESS : CL_INVALID_PLATFORM; }
static cl_int UnloadCompiler(void) { return CL_SUCCESS; }

// The kernels' names, ;-separated (a new string).
static char *kernel_names(cl_program p) {
    struct request r = {KERNELS, {p->handle, 0, 0}};
    if (call(&r) != CL_SUCCESS)
        return NULL;
    char *s = calloc(1, r.r[0] + 1);
    r = (struct request){KERNELS, {p->handle, (uintptr_t)s, r.r[0]}};
    if (s && call(&r) != CL_SUCCESS) {
        free(s);
        return NULL;
    }
    return s;
}

static cl_int GetProgramInfo(cl_program p, cl_program_info what, size_t cap, void *out, size_t *size_ret) {
    if (!p)
        return CL_INVALID_PROGRAM;
    switch (what) {
    case CL_PROGRAM_REFERENCE_COUNT:
        INFO_OF(cl_uint, p->refs);
    case CL_PROGRAM_CONTEXT:
        INFO_OF(cl_context, p->ctx);
    case CL_PROGRAM_NUM_DEVICES:
        INFO_OF(cl_uint, 1);
    case CL_PROGRAM_DEVICES:
        INFO_OF(cl_device_id, &device);
    case CL_PROGRAM_SOURCE:
        return info(p->source, p->len + 1, cap, out, size_ret);
    case CL_PROGRAM_BINARY_SIZES:
        INFO_OF(size_t, p->len);
    case CL_PROGRAM_BINARIES:
        if (out) {
            if (cap < sizeof(char *))
                return CL_INVALID_VALUE;
            unsigned char *b = *(unsigned char **)out;
            if (b)
                memcpy(b, p->source, p->len);
        }
        if (size_ret)
            *size_ret = sizeof(char *);
        return CL_SUCCESS;
    case CL_PROGRAM_NUM_KERNELS:
    case CL_PROGRAM_KERNEL_NAMES: {
        if (p->status != CL_BUILD_SUCCESS)
            return CL_INVALID_PROGRAM_EXECUTABLE;
        char *names = kernel_names(p);
        if (!names)
            return CL_OUT_OF_RESOURCES;
        cl_int e;
        if (what == CL_PROGRAM_KERNEL_NAMES) {
            e = info(names, strlen(names) + 1, cap, out, size_ret);
        } else {
            size_t count = *names ? 1 : 0;
            for (char *c = names; *c; c++)
                count += *c == ';';
            e = info(&count, sizeof count, cap, out, size_ret);
        }
        free(names);
        return e;
    }
    }
    return CL_INVALID_VALUE;
}

static cl_int GetProgramBuildInfo(cl_program p, cl_device_id d, cl_program_build_info what, size_t cap, void *out, size_t *size_ret) {
    if (!p)
        return CL_INVALID_PROGRAM;
    if (d != &device)
        return CL_INVALID_DEVICE;
    switch (what) {
    case CL_PROGRAM_BUILD_STATUS:
        INFO_OF(cl_build_status, p->status);
    case CL_PROGRAM_BUILD_OPTIONS:
        INFO_STR(p->options ? p->options : "");
    case CL_PROGRAM_BINARY_TYPE:
        INFO_OF(cl_program_binary_type, p->status == CL_BUILD_SUCCESS ? CL_PROGRAM_BINARY_TYPE_EXECUTABLE : CL_PROGRAM_BINARY_TYPE_NONE);
    case CL_PROGRAM_BUILD_LOG: {
        if (!p->handle)
            INFO_STR("");
        struct request r = {LOG, {p->handle, 0, 0}};
        if (call(&r) != CL_SUCCESS)
            INFO_STR("");
        size_t n = r.r[0];
        char *s = calloc(1, n + 1);
        if (!s)
            return CL_OUT_OF_HOST_MEMORY;
        r = (struct request){LOG, {p->handle, (uintptr_t)s, n}};
        call(&r);
        cl_int e = info(s, n + 1, cap, out, size_ret);
        free(s);
        return e;
    }
    }
    return CL_INVALID_VALUE;
}

// ---- Kernels ----

static cl_kernel CreateKernel(cl_program p, const char *name, cl_int *err) {
    cl_int e = !p ? CL_INVALID_PROGRAM : p->status != CL_BUILD_SUCCESS ? CL_INVALID_PROGRAM_EXECUTABLE : !name ? CL_INVALID_VALUE : CL_SUCCESS;
    cl_kernel k = NULL;
    if (e == CL_SUCCESS) {
        struct request r = {ARGS, {p->handle, (uintptr_t)name, strlen(name), 0, 0}};
        e = call(&r);
        size_t n = r.r[0], block = r.r[2];
        k = e == CL_SUCCESS ? calloc(1, sizeof *k) : NULL;
        if (k) {
            *k = (struct _cl_kernel){&dispatch, 1, p, dup_bytes(name, strlen(name)), (cl_uint)n};
            k->args = calloc(n + 1, sizeof *k->args);
            k->block = calloc(1, block + 4);
            k->block_size = block;
            k->handles = calloc(n + 1, sizeof *k->handles);
            k->locals = calloc(n + 1, sizeof *k->locals);
            k->set = calloc(n + 1, 1);
            k->required[0] = r.r[3], k->required[1] = r.r[4], k->required[2] = r.r[5];
            r = (struct request){ARGS, {p->handle, (uintptr_t)name, strlen(name), (uintptr_t)k->args, n * sizeof *k->args}};
            if (!k->name || !k->args || !k->block || !k->handles || !k->locals || !k->set || call(&r) != CL_SUCCESS) {
                e = CL_OUT_OF_HOST_MEMORY;
                free(k->name), free(k->args), free(k->block), free(k->handles), free(k->locals), free(k->set), free(k);
                k = NULL;
            } else {
                p->refs++;
                p->kernels++;
            }
        } else if (e == CL_SUCCESS) {
            e = CL_OUT_OF_HOST_MEMORY;
        }
    }
    if (err)
        *err = e;
    return k;
}

static cl_int CreateKernelsInProgram(cl_program p, cl_uint n, cl_kernel *kernels, cl_uint *count) {
    if (!p)
        return CL_INVALID_PROGRAM;
    if (p->status != CL_BUILD_SUCCESS)
        return CL_INVALID_PROGRAM_EXECUTABLE;
    char *names = kernel_names(p);
    if (!names)
        return CL_OUT_OF_RESOURCES;
    cl_uint total = 0;
    cl_int e = CL_SUCCESS;
    for (char *s = names; *s;) {
        char *end = strchr(s, ';');
        if (end)
            *end = 0;
        if (kernels) {
            if (total >= n) {
                e = CL_INVALID_VALUE;
                break;
            }
            kernels[total] = CreateKernel(p, s, &e);
            if (e != CL_SUCCESS)
                break;
        }
        total++;
        if (!end)
            break;
        s = end + 1;
    }
    free(names);
    if (count)
        *count = total;
    return e;
}

static cl_int RetainKernel(cl_kernel k) {
    if (!k)
        return CL_INVALID_KERNEL;
    k->refs++;
    return CL_SUCCESS;
}

static cl_int ReleaseKernel(cl_kernel k) {
    if (!k)
        return CL_INVALID_KERNEL;
    if (--k->refs == 0) {
        k->program->kernels--;
        ReleaseProgram(k->program);
        free(k->name), free(k->args), free(k->block), free(k->handles), free(k->locals), free(k->set), free(k);
    }
    return CL_SUCCESS;
}

static cl_int SetKernelArg(cl_kernel k, cl_uint i, size_t size, const void *value) {
    if (!k)
        return CL_INVALID_KERNEL;
    if (i >= k->nargs)
        return CL_INVALID_ARG_INDEX;
    struct arg *a = &k->args[i];
    switch (a->kind) {
    case 0:
    case 1: {
        if (size != sizeof(cl_mem))
            return CL_INVALID_ARG_SIZE;
        cl_mem m = value ? *(const cl_mem *)value : NULL;
        // A null buffer argument: the kernel mustn't use it; offset 0 in
        // no buffer (the device reports it if it does).
        k->handles[i] = m ? base_handle(m) : 0;
        uint32_t offset = m ? (uint32_t)base_offset(m) : 0;
        memcpy(k->block + a->offset, &offset, 4);
        break;
    }
    case 2:
        if (value)
            return CL_INVALID_ARG_VALUE;
        if (!size)
            return CL_INVALID_ARG_SIZE;
        k->locals[i] = (uint32_t)size;
        break;
    default:
        if (size != a->size)
            return CL_INVALID_ARG_SIZE;
        if (!value)
            return CL_INVALID_ARG_VALUE;
        memcpy(k->block + a->offset, value, size);
    }
    k->set[i] = 1;
    return CL_SUCCESS;
}

static cl_int GetKernelInfo(cl_kernel k, cl_kernel_info what, size_t cap, void *out, size_t *size_ret) {
    if (!k)
        return CL_INVALID_KERNEL;
    switch (what) {
    case CL_KERNEL_FUNCTION_NAME:
        INFO_STR(k->name);
    case CL_KERNEL_NUM_ARGS:
        INFO_OF(cl_uint, k->nargs);
    case CL_KERNEL_REFERENCE_COUNT:
        INFO_OF(cl_uint, k->refs);
    case CL_KERNEL_CONTEXT:
        INFO_OF(cl_context, k->program->ctx);
    case CL_KERNEL_PROGRAM:
        INFO_OF(cl_program, k->program);
    case CL_KERNEL_ATTRIBUTES:
        INFO_STR("");
    }
    return CL_INVALID_VALUE;
}

static cl_int GetKernelWorkGroupInfo(cl_kernel k, cl_device_id d, cl_kernel_work_group_info what, size_t cap, void *out, size_t *size_ret) {
    if (!k)
        return CL_INVALID_KERNEL;
    if (d && d != &device)
        return CL_INVALID_DEVICE;
    switch (what) {
    case CL_KERNEL_WORK_GROUP_SIZE:
        INFO_OF(size_t, desc.max_group);
    case CL_KERNEL_COMPILE_WORK_GROUP_SIZE:
        return info(k->required, sizeof k->required, cap, out, size_ret);
    case CL_KERNEL_LOCAL_MEM_SIZE:
    case CL_KERNEL_PRIVATE_MEM_SIZE:
        INFO_OF(cl_ulong, 0);
    case CL_KERNEL_PREFERRED_WORK_GROUP_SIZE_MULTIPLE:
        INFO_OF(size_t, 32);
    }
    return CL_INVALID_VALUE;
}

static cl_int GetKernelArgInfo(cl_kernel k, cl_uint i, cl_kernel_arg_info what, size_t cap, void *out, size_t *size_ret) {
    if (!k)
        return CL_INVALID_KERNEL;
    if (i >= k->nargs)
        return CL_INVALID_ARG_INDEX;
    struct arg *a = &k->args[i];
    switch (what) {
    case CL_KERNEL_ARG_ADDRESS_QUALIFIER:
        INFO_OF(cl_kernel_arg_address_qualifier,
                a->kind == 0   ? CL_KERNEL_ARG_ADDRESS_GLOBAL
                : a->kind == 1 ? CL_KERNEL_ARG_ADDRESS_CONSTANT
                : a->kind == 2 ? CL_KERNEL_ARG_ADDRESS_LOCAL
                               : CL_KERNEL_ARG_ADDRESS_PRIVATE);
    case CL_KERNEL_ARG_ACCESS_QUALIFIER:
        INFO_OF(cl_kernel_arg_access_qualifier, CL_KERNEL_ARG_ACCESS_NONE);
    case CL_KERNEL_ARG_TYPE_NAME:
        INFO_STR(a->type);
    case CL_KERNEL_ARG_TYPE_QUALIFIER:
        INFO_OF(cl_kernel_arg_type_qualifier, a->kind == 1 ? CL_KERNEL_ARG_TYPE_CONST : CL_KERNEL_ARG_TYPE_NONE);
    case CL_KERNEL_ARG_NAME:
        if (!a->name[0])
            return CL_KERNEL_ARG_INFO_NOT_AVAILABLE;
        INFO_STR(a->name);
    }
    return CL_INVALID_VALUE;
}

static cl_int EnqueueNDRangeKernel(cl_command_queue q, cl_kernel k, cl_uint dims, const size_t *offset, const size_t *global, const size_t *local,
                                   cl_uint nw, const cl_event *wait, cl_event *ev) {
    (void)nw, (void)wait;
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!k)
        return CL_INVALID_KERNEL;
    if (dims < 1 || dims > 3)
        return CL_INVALID_WORK_DIMENSION;
    if (!global)
        return CL_INVALID_GLOBAL_WORK_SIZE;
    for (cl_uint i = 0; i < k->nargs; i++)
        if (!k->set[i])
            return CL_INVALID_KERNEL_ARGS;
    uint32_t sizes[9] = {0};
    for (cl_uint d = 0; d < dims; d++) {
        if (!global[d] || global[d] > 0xffffffffu)
            return CL_INVALID_GLOBAL_WORK_SIZE;
        sizes[d] = (uint32_t)global[d];
        sizes[3 + d] = offset ? (uint32_t)offset[d] : 0;
        sizes[6 + d] = local ? (uint32_t)local[d] : 0;
        if (local && (!local[d] || global[d] % local[d]))
            return CL_INVALID_WORK_GROUP_SIZE;
    }
    struct request r = {RUN,
                        {k->program->handle, (uintptr_t)k->name, strlen(k->name), dims, (uintptr_t)sizes, (uintptr_t)k->block, k->block_size,
                         (uintptr_t)k->handles, (uintptr_t)k->locals}};
    cl_int e = call(&r);
    return e != CL_SUCCESS ? e : done(q, CL_COMMAND_NDRANGE_KERNEL, ev);
}

static cl_int EnqueueTask(cl_command_queue q, cl_kernel k, cl_uint nw, const cl_event *wait, cl_event *ev) {
    size_t one = 1;
    return EnqueueNDRangeKernel(q, k, 1, NULL, &one, &one, nw, wait, ev);
}

// ---- Events and synchronization ----

static cl_int EnqueueMarkerWithWaitList(cl_command_queue q, cl_uint nw, const cl_event *wait, cl_event *ev) {
    (void)nw, (void)wait;
    return q ? done(q, CL_COMMAND_MARKER, ev) : CL_INVALID_COMMAND_QUEUE;
}

static cl_int EnqueueBarrierWithWaitList(cl_command_queue q, cl_uint nw, const cl_event *wait, cl_event *ev) {
    (void)nw, (void)wait;
    return q ? done(q, CL_COMMAND_BARRIER, ev) : CL_INVALID_COMMAND_QUEUE;
}

static cl_int EnqueueMarker(cl_command_queue q, cl_event *ev) { return EnqueueMarkerWithWaitList(q, 0, NULL, ev); }
static cl_int EnqueueBarrier(cl_command_queue q) { return q ? CL_SUCCESS : CL_INVALID_COMMAND_QUEUE; }
static cl_int EnqueueWaitForEvents(cl_command_queue q, cl_uint n, const cl_event *events) {
    (void)n, (void)events;
    return q ? CL_SUCCESS : CL_INVALID_COMMAND_QUEUE;
}

static cl_int WaitForEvents(cl_uint n, const cl_event *events) {
    if (!n || !events)
        return CL_INVALID_VALUE;
    for (cl_uint i = 0; i < n; i++)
        if (events[i]->status < 0)
            return CL_EXEC_STATUS_ERROR_FOR_EVENTS_IN_WAIT_LIST;
    struct request r = {FINISH};
    return call(&r);
}

static cl_int GetEventInfo(cl_event e, cl_event_info what, size_t cap, void *out, size_t *size_ret) {
    if (!e)
        return CL_INVALID_EVENT;
    switch (what) {
    case CL_EVENT_COMMAND_QUEUE:
        INFO_OF(cl_command_queue, e->queue);
    case CL_EVENT_CONTEXT:
        INFO_OF(cl_context, e->ctx);
    case CL_EVENT_COMMAND_TYPE:
        INFO_OF(cl_command_type, e->type);
    case CL_EVENT_COMMAND_EXECUTION_STATUS:
        INFO_OF(cl_int, e->status);
    case CL_EVENT_REFERENCE_COUNT:
        INFO_OF(cl_uint, e->refs);
    }
    return CL_INVALID_VALUE;
}

static cl_int GetEventProfilingInfo(cl_event e, cl_profiling_info what, size_t cap, void *out, size_t *size_ret) {
    if (!e)
        return CL_INVALID_EVENT;
    if (!e->queue || !(e->queue->props & CL_QUEUE_PROFILING_ENABLE))
        return CL_PROFILING_INFO_NOT_AVAILABLE;
    // Commands run as they're enqueued: no times to tell apart.
    switch (what) {
    case CL_PROFILING_COMMAND_QUEUED:
    case CL_PROFILING_COMMAND_SUBMIT:
    case CL_PROFILING_COMMAND_START:
    case CL_PROFILING_COMMAND_END:
    case CL_PROFILING_COMMAND_COMPLETE:
        INFO_OF(cl_ulong, 0);
    }
    return CL_INVALID_VALUE;
}

static cl_event CreateUserEvent(cl_context c, cl_int *err) {
    cl_event e = c ? event(NULL, CL_COMMAND_USER) : NULL;
    if (e) {
        e->ctx = c;
        e->status = CL_SUBMITTED;
        c->refs++;
    }
    if (err)
        *err = !c ? CL_INVALID_CONTEXT : e ? CL_SUCCESS : CL_OUT_OF_HOST_MEMORY;
    return e;
}

static cl_int SetUserEventStatus(cl_event e, cl_int status) {
    if (!e || e->type != CL_COMMAND_USER)
        return CL_INVALID_EVENT;
    e->status = status;
    return CL_SUCCESS;
}

static cl_int SetEventCallback(cl_event e, cl_int type, void(CL_CALLBACK *notify)(cl_event, cl_int, void *), void *user) {
    if (!e)
        return CL_INVALID_EVENT;
    if (!notify)
        return CL_INVALID_VALUE;
    // Events are complete when made (user events aside): the callback runs now.
    if (e->status <= type)
        notify(e, e->status, user);
    return CL_SUCCESS;
}

static cl_int RetainEvent(cl_event e) {
    if (!e)
        return CL_INVALID_EVENT;
    e->refs++;
    return CL_SUCCESS;
}

static cl_int ReleaseEvent(cl_event e) {
    if (!e)
        return CL_INVALID_EVENT;
    if (--e->refs == 0) {
        if (e->queue)
            ReleaseCommandQueue(e->queue);
        else if (e->ctx)
            ReleaseContext(e->ctx);
        free(e);
    }
    return CL_SUCCESS;
}

// ---- The dispatch table ----

static void *GetExtensionFunctionAddress(const char *name) {
    if (name && !strcmp(name, "clIcdGetPlatformIDsKHR"))
        return (void *)clIcdGetPlatformIDsKHR;
    // (ocl-icd asks for this one by name too.)
    if (name && !strcmp(name, "clGetPlatformInfo"))
        return (void *)GetPlatformInfo;
    return NULL;
}

static void *GetExtensionFunctionAddressForPlatform(cl_platform_id p, const char *name) {
    (void)p;
    return GetExtensionFunctionAddress(name);
}

// Whatever isn't implemented.
static cl_int unsupported(void) { return CL_INVALID_OPERATION; }

API void *clGetExtensionFunctionAddress(const char *name) { return GetExtensionFunctionAddress(name); }

__attribute__((constructor)) static void init(void) {
    dispatch = (struct _cl_icd_dispatch){
        .clGetPlatformInfo = GetPlatformInfo,
        .clGetDeviceIDs = GetDeviceIDs,
        .clGetDeviceInfo = GetDeviceInfo,
        .clCreateContext = CreateContext,
        .clCreateContextFromType = CreateContextFromType,
        .clRetainContext = RetainContext,
        .clReleaseContext = ReleaseContext,
        .clGetContextInfo = GetContextInfo,
        .clCreateCommandQueue = CreateCommandQueue,
        .clRetainCommandQueue = RetainCommandQueue,
        .clReleaseCommandQueue = ReleaseCommandQueue,
        .clGetCommandQueueInfo = GetCommandQueueInfo,
        .clCreateBuffer = CreateBuffer,
        .clRetainMemObject = RetainMemObject,
        .clReleaseMemObject = ReleaseMemObject,
        .clGetSupportedImageFormats = GetSupportedImageFormats,
        .clGetMemObjectInfo = GetMemObjectInfo,
        .clCreateProgramWithSource = CreateProgramWithSource,
        .clCreateProgramWithBinary = CreateProgramWithBinary,
        .clRetainProgram = RetainProgram,
        .clReleaseProgram = ReleaseProgram,
        .clBuildProgram = BuildProgram,
        .clUnloadCompiler = UnloadCompiler,
        .clGetProgramInfo = GetProgramInfo,
        .clGetProgramBuildInfo = GetProgramBuildInfo,
        .clCreateKernel = CreateKernel,
        .clCreateKernelsInProgram = CreateKernelsInProgram,
        .clRetainKernel = RetainKernel,
        .clReleaseKernel = ReleaseKernel,
        .clSetKernelArg = SetKernelArg,
        .clGetKernelInfo = GetKernelInfo,
        .clGetKernelWorkGroupInfo = GetKernelWorkGroupInfo,
        .clWaitForEvents = WaitForEvents,
        .clGetEventInfo = GetEventInfo,
        .clRetainEvent = RetainEvent,
        .clReleaseEvent = ReleaseEvent,
        .clGetEventProfilingInfo = GetEventProfilingInfo,
        .clFlush = Flush,
        .clFinish = Finish,
        .clEnqueueReadBuffer = EnqueueReadBuffer,
        .clEnqueueWriteBuffer = EnqueueWriteBuffer,
        .clEnqueueCopyBuffer = EnqueueCopyBuffer,
        .clEnqueueMapBuffer = EnqueueMapBuffer,
        .clEnqueueUnmapMemObject = EnqueueUnmapMemObject,
        .clEnqueueNDRangeKernel = EnqueueNDRangeKernel,
        .clEnqueueTask = EnqueueTask,
        .clEnqueueMarker = EnqueueMarker,
        .clEnqueueWaitForEvents = EnqueueWaitForEvents,
        .clEnqueueBarrier = EnqueueBarrier,
        .clGetExtensionFunctionAddress = GetExtensionFunctionAddress,
        .clCreateSubBuffer = CreateSubBuffer,
        .clSetEventCallback = SetEventCallback,
        .clCreateUserEvent = CreateUserEvent,
        .clSetUserEventStatus = SetUserEventStatus,
        .clEnqueueReadBufferRect = EnqueueReadBufferRect,
        .clEnqueueWriteBufferRect = EnqueueWriteBufferRect,
        .clRetainDevice = RetainDevice,
        .clReleaseDevice = ReleaseDevice,
        .clCompileProgram = CompileProgram,
        .clLinkProgram = LinkProgram,
        .clUnloadPlatformCompiler = UnloadPlatformCompiler,
        .clGetKernelArgInfo = GetKernelArgInfo,
        .clEnqueueFillBuffer = EnqueueFillBuffer,
        .clEnqueueMarkerWithWaitList = EnqueueMarkerWithWaitList,
        .clEnqueueBarrierWithWaitList = EnqueueBarrierWithWaitList,
        .clGetExtensionFunctionAddressForPlatform = GetExtensionFunctionAddressForPlatform,
        .clCreateCommandQueueWithProperties = CreateCommandQueueWithProperties,
    };
    // The rest fail cleanly rather than crash the loader's call.
    void **slots = (void **)&dispatch;
    for (size_t i = 0; i < sizeof dispatch / sizeof(void *); i++)
        if (!slots[i])
            slots[i] = (void *)unsupported;
    // (clGetPlatformIDs is the loader's own; its slot marks the table as
    // the classic kind.)
    dispatch.clGetPlatformIDs = NULL;
}
