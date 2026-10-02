// wasm-env's OpenCL driver: an installable client driver (ICD) for the
// Khronos loader (Ubuntu's ocl-icd), implementing OpenCL 1.2 on the
// runtime's GPU service through /dev/wasm-env-gpu (runtime/src/gpu.rs has
// the requests). One platform, one device. Commands run in order as
// they're enqueued, so their events are complete when made, but for those
// waiting on events not done yet (user events): those wait (`command`).
// Reads wait for the work before them.
//
// It uses nothing from the C library but allocation, a few string
// functions, open, ioctl, close and sched_yield, resolved when it's loaded:
// the same library serves glibc and musl programs.
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
char *strstr(const char *, const char *);
int memcmp(const void *, const void *, size_t);
int open(const char *, int, ...);
int ioctl(int, unsigned long, ...);
int close(int);
int sched_yield(void);
struct timespec {
    long tv_sec, tv_nsec;
};
int clock_gettime(int, struct timespec *);
#define CLOCK_MONOTONIC 1
#define O_RDWR 02
#define O_CLOEXEC 02000000

#define API __attribute__((visibility("default")))
#define GPU_CALL 0xc0805740u

enum { INFO = 1, CREATE, FREE, WRITE, READ, COPY, FILL, BUILD, LOG, KERNELS, ARGS, RUN, FINISH, ATTRIBUTES, COMPILE, LINK, BINARY, LOAD };

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
    // Its commands waiting (`command`), whether one is running, and the
    // last scan of them that saw it (under the commands' lock).
    cl_uint pending;
    int running;
    unsigned scan;
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
    // Destructor callbacks, the latest first (the order they run in).
    struct callback {
        void(CL_CALLBACK *f)(cl_mem, void *);
        void *user;
        struct callback *next;
    } *callbacks;
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
    // An executable, a compiled object or a library (bitcode in the runtime).
    cl_program_binary_type binary;
    // Linked (or loaded from a linked binary): nothing to build, and its
    // binary is its bitcode.
    int prebuilt;
};
struct arg {
    uint32_t kind, size, offset, qualifiers;
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
    char *attributes;
    size_t static_local;
};
struct _cl_event {
    struct _cl_icd_dispatch *d;
    cl_uint refs;
    cl_context ctx;
    cl_command_queue queue;
    cl_command_type type;
    cl_int status;
    // When it was enqueued (its work began) and when it finished.
    cl_ulong began, ended;
    // Callbacks waiting for a status (user events').
    struct waiting {
        void(CL_CALLBACK *f)(cl_event, cl_int, void *);
        void *user;
        cl_int type;
        struct waiting *next;
    } *waiting;
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

static cl_ulong now(void) {
    struct timespec t = {0, 0};
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (cl_ulong)t.tv_sec * 1000000000u + (cl_ulong)t.tv_nsec;
}

// When the command being enqueued began: its first request to the device.
static _Thread_local cl_ulong began;
static _Thread_local int beginning = 1;

// The API is called from many threads at once (the spec allows it, the CTS
// does it): reference counts are atomic, and so is what's shared.
static void retain(cl_uint *refs) { __atomic_add_fetch(refs, 1, __ATOMIC_RELAXED); }
static cl_uint release(cl_uint *refs) { return __atomic_sub_fetch(refs, 1, __ATOMIC_ACQ_REL); }

static int call(struct request *r) {
    if (beginning) {
        began = now();
        beginning = 0;
    }
    return ioctl(gpu, GPU_CALL, r) == 0 ? (int)(int64_t)r->r[1] : CL_OUT_OF_RESOURCES;
}

// The device, opened once: 0 not yet, 1 opening, 2 open, 3 none.
static int gpu_state;

static int open_gpu(void) {
    int s = __atomic_load_n(&gpu_state, __ATOMIC_ACQUIRE);
    if (s == 0 && __atomic_compare_exchange_n(&gpu_state, &s, 1, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
        int fd = open("/dev/wasm-env-gpu", O_RDWR | O_CLOEXEC);
        int ok = fd >= 0;
        if (ok) {
            gpu = fd;
            struct request r = {INFO, {(uintptr_t)&desc, sizeof desc}};
            ok = call(&r) == CL_SUCCESS;
            if (!ok) {
                close(fd);
                gpu = -1;
            }
        }
        __atomic_store_n(&gpu_state, ok ? 2 : 3, __ATOMIC_RELEASE);
        return ok;
    }
    // (Another thread is opening it: wait for that.)
    while ((s = __atomic_load_n(&gpu_state, __ATOMIC_ACQUIRE)) == 1) {
    }
    return s == 2;
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
        e->ended = now();
        e->began = beginning ? e->ended : began;
        if (q)
            retain(&q->refs);
    }
    return e;
}

static cl_int done(cl_command_queue q, cl_command_type type, cl_event *ev) {
    cl_int e = CL_SUCCESS;
    if (ev && !(*ev = event(q, type)))
        e = CL_OUT_OF_HOST_MEMORY;
    // (The next request begins another command.)
    beginning = 1;
    return e;
}

// ---- Commands ----

// A command runs as it's enqueued, unless an event in its wait list isn't
// done yet (a user event's, or a command's still waiting) or its queue has
// commands waiting (queues run in order): then it waits, its event queued,
// and runs once they're done (`drain`), or fails if one of them failed.
struct command {
    cl_int (*run)(struct command *);
    uintptr_t a[8];
    // What it owns (copies made when it was enqueued), and the objects it
    // uses (retained while it waits).
    void *data;
    cl_mem mem[2];
    cl_kernel kernel;
    // Waiting: its queue, its event and its wait list (retained).
    cl_command_queue q;
    cl_event ev;
    cl_uint nw;
    cl_event *wait;
    struct command *next;
};

static struct command *commands;
static int commands_lock;
static void lock_commands(void) {
    while (__atomic_exchange_n(&commands_lock, 1, __ATOMIC_ACQUIRE))
        sched_yield();
}
static void unlock_commands(void) { __atomic_store_n(&commands_lock, 0, __ATOMIC_RELEASE); }

static cl_int ReleaseMemObject(cl_mem m);
static cl_int ReleaseKernel(cl_kernel k);
static cl_int ReleaseEvent(cl_event e);

// A wait list: well formed, and events of the queue's context.
static cl_int waits(cl_command_queue q, cl_uint n, const cl_event *wait) {
    if ((n && !wait) || (!n && wait))
        return CL_INVALID_EVENT_WAIT_LIST;
    for (cl_uint i = 0; i < n; i++) {
        if (!wait[i])
            return CL_INVALID_EVENT_WAIT_LIST;
        if (wait[i]->ctx != q->ctx)
            return CL_INVALID_CONTEXT;
    }
    return CL_SUCCESS;
}

static cl_int status_of(cl_event e) { return __atomic_load_n(&e->status, __ATOMIC_ACQUIRE); }

// Whether a wait list's events are all done (complete, or failed: then
// `failed` is set).
static int settled(cl_uint n, const cl_event *wait, int *failed) {
    for (cl_uint i = 0; i < n; i++) {
        cl_int s = status_of(wait[i]);
        if (s > CL_COMPLETE)
            return 0;
        if (s < 0)
            *failed = 1;
    }
    return 1;
}

// An event's new status, and the callbacks waiting for it (complete, or an
// error: past running).
static void set_status(cl_event e, cl_int status) {
    lock_commands();
    __atomic_store_n(&e->status, status, __ATOMIC_RELEASE);
    struct waiting *due = NULL;
    for (struct waiting **at = &e->waiting; *at;) {
        struct waiting *w = *at;
        if (status <= w->type) {
            *at = w->next;
            w->next = due;
            due = w;
        } else {
            at = &w->next;
        }
    }
    unlock_commands();
    while (due) {
        struct waiting *w = due;
        due = w->next;
        w->f(e, status, w->user);
        free(w);
    }
}

// What a command owns, once it has run.
static void drop(struct command *c) {
    free(c->data);
    c->data = NULL;
}

// A command that can't run yet, queued (the caller holds the lock). `held`
// takes a reference to its event too (for a blocking call to wait on).
static cl_int defer(cl_command_queue q, cl_command_type type, struct command *c, cl_uint nw, const cl_event *wait, cl_event *ev, cl_event *held) {
    struct command *d = malloc(sizeof *d);
    cl_event e = event(q, type);
    cl_event *w = nw ? malloc(nw * sizeof *w) : NULL;
    if (!d || !e || (nw && !w)) {
        free(d);
        free(w);
        if (e)
            ReleaseEvent(e);
        drop(c);
        return CL_OUT_OF_HOST_MEMORY;
    }
    *d = *c;
    e->status = CL_QUEUED;
    for (cl_uint i = 0; i < nw; i++) {
        w[i] = wait[i];
        retain(&w[i]->refs);
    }
    for (int i = 0; i < 2; i++)
        if (d->mem[i])
            retain(&d->mem[i]->refs);
    if (d->kernel)
        retain(&d->kernel->refs);
    d->q = q;
    d->ev = e;
    d->nw = nw;
    d->wait = w;
    d->next = NULL;
    struct command **at = &commands;
    while (*at)
        at = &(*at)->next;
    *at = d;
    q->pending++;
    if (ev) {
        retain(&e->refs);
        *ev = e;
    }
    if (held) {
        retain(&e->refs);
        *held = e;
    }
    beginning = 1;
    return CL_SUCCESS;
}

// Runs the waiting commands that can run now: the first waiting of each
// queue, once its wait list is done. (Each that finishes can let others
// run.)
static void drain(void) {
    static unsigned scan;
    for (;;) {
        struct command *c = NULL;
        int failed = 0;
        lock_commands();
        scan++;
        for (struct command **at = &commands; *at; at = &(*at)->next) {
            struct command *x = *at;
            if (x->q->scan == scan)
                continue;
            x->q->scan = scan;
            if (!x->q->running && settled(x->nw, x->wait, &failed)) {
                c = x;
                *at = x->next;
                x->q->running = 1;
                break;
            }
            failed = 0;
        }
        unlock_commands();
        if (!c)
            return;
        cl_int status = CL_EXEC_STATUS_ERROR_FOR_EVENTS_IN_WAIT_LIST;
        if (!failed) {
            set_status(c->ev, CL_RUNNING);
            beginning = 1;
            cl_ulong start = now();
            cl_int e = c->run(c);
            c->ev->began = beginning ? start : began;
            c->ev->ended = now();
            beginning = 1;
            status = e == CL_SUCCESS ? CL_COMPLETE : e;
        }
        set_status(c->ev, status);
        lock_commands();
        c->q->running = 0;
        c->q->pending--;
        unlock_commands();
        drop(c);
        for (cl_uint i = 0; i < c->nw; i++)
            ReleaseEvent(c->wait[i]);
        free(c->wait);
        for (int i = 0; i < 2; i++)
            if (c->mem[i])
                ReleaseMemObject(c->mem[i]);
        if (c->kernel)
            ReleaseKernel(c->kernel);
        ReleaseEvent(c->ev);
        free(c);
    }
}

// Until an event is done: CL_SUCCESS, or the error for a failed one.
static cl_int wait_for(cl_event e) {
    while (status_of(e) > CL_COMPLETE) {
        drain();
        sched_yield();
    }
    return status_of(e) < 0 ? CL_EXEC_STATUS_ERROR_FOR_EVENTS_IN_WAIT_LIST : CL_SUCCESS;
}

// A command enqueued: run now, or waiting (see `command`). A blocking one
// returns once it has run.
static cl_int submit(cl_command_queue q, cl_command_type type, struct command *c, cl_uint nw, const cl_event *wait, cl_event *ev, int blocking) {
    cl_int e = waits(q, nw, wait);
    if (e != CL_SUCCESS) {
        drop(c);
        return e;
    }
    int failed = 0;
    lock_commands();
    if (q->pending || !settled(nw, wait, &failed)) {
        cl_event held = NULL;
        e = defer(q, type, c, nw, wait, ev, blocking ? &held : NULL);
        unlock_commands();
        if (e != CL_SUCCESS || !held)
            return e;
        e = wait_for(held);
        ReleaseEvent(held);
        return e;
    }
    unlock_commands();
    if (failed) {
        // (Not run: its event says why.)
        drop(c);
        if (ev && (*ev = event(q, type)))
            (*ev)->status = CL_EXEC_STATUS_ERROR_FOR_EVENTS_IN_WAIT_LIST;
        beginning = 1;
        return blocking ? CL_EXEC_STATUS_ERROR_FOR_EVENTS_IN_WAIT_LIST : CL_SUCCESS;
    }
    e = c->run(c);
    drop(c);
    if (e != CL_SUCCESS) {
        beginning = 1;
        return e;
    }
    return done(q, type, ev);
}

static cl_int run_nothing(struct command *c) {
    (void)c;
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
    const cl_device_type known = CL_DEVICE_TYPE_DEFAULT | CL_DEVICE_TYPE_CPU | CL_DEVICE_TYPE_GPU | CL_DEVICE_TYPE_ACCELERATOR | CL_DEVICE_TYPE_CUSTOM;
    if (type != CL_DEVICE_TYPE_ALL && (!type || (type & ~known)))
        return CL_INVALID_DEVICE_TYPE;
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
                                 "cl_khr_local_int32_extended_atomics cl_khr_byte_addressable_store cl_khr_fp64 cl_khr_fp16";

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
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_DOUBLE:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_DOUBLE:
    case CL_DEVICE_PREFERRED_VECTOR_WIDTH_HALF:
    case CL_DEVICE_NATIVE_VECTOR_WIDTH_HALF:
        INFO_OF(cl_uint, 1);
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
    // (Doubles: the runtime's, IEEE in software where the device's aren't.)
    case CL_DEVICE_DOUBLE_FP_CONFIG:
        INFO_OF(cl_device_fp_config, CL_FP_FMA | CL_FP_ROUND_TO_NEAREST | CL_FP_ROUND_TO_ZERO | CL_FP_ROUND_TO_INF | CL_FP_INF_NAN | CL_FP_DENORM);
    // (Halves: floats rounded to half precision, denormals and all.)
    case CL_DEVICE_HALF_FP_CONFIG:
        INFO_OF(cl_device_fp_config, CL_FP_ROUND_TO_NEAREST | CL_FP_INF_NAN | CL_FP_DENORM);
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
        // (Out of order allowed: commands running in order is one such order.)
        INFO_OF(cl_command_queue_properties, CL_QUEUE_PROFILING_ENABLE | CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE);
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
    if (!n || !devices || (!notify && user))
        e = CL_INVALID_VALUE;
    for (cl_uint i = 0; e == CL_SUCCESS && i < n; i++)
        if (devices[i] != &device)
            e = CL_INVALID_DEVICE;
    // Properties: the platform (ours), and interop sync (accepted).
    for (size_t i = 0; e == CL_SUCCESS && props && props[i]; i += 2) {
        if (props[i] == CL_CONTEXT_PLATFORM) {
            if ((cl_platform_id)props[i + 1] != &platform)
                e = CL_INVALID_PLATFORM;
        } else if (props[i] == CL_CONTEXT_INTEROP_USER_SYNC) {
            if (props[i + 1] != CL_TRUE && props[i + 1] != CL_FALSE)
                e = CL_INVALID_PROPERTY;
        } else {
            e = CL_INVALID_PROPERTY;
        }
        for (size_t j = 0; e == CL_SUCCESS && j < i; j += 2)
            if (props[j] == props[i])
                e = CL_INVALID_PROPERTY;
    }
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
    const cl_device_type known = CL_DEVICE_TYPE_DEFAULT | CL_DEVICE_TYPE_CPU | CL_DEVICE_TYPE_GPU | CL_DEVICE_TYPE_ACCELERATOR | CL_DEVICE_TYPE_CUSTOM;
    if (type != CL_DEVICE_TYPE_ALL && (!type || (type & ~known))) {
        if (err)
            *err = CL_INVALID_DEVICE_TYPE;
        return NULL;
    }
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
    retain(&c->refs);
    return CL_SUCCESS;
}

static cl_int ReleaseContext(cl_context c) {
    if (!c)
        return CL_INVALID_CONTEXT;
    if (release(&c->refs) == 0)
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
    cl_int e = !c ? CL_INVALID_CONTEXT : d != &device ? CL_INVALID_DEVICE : props & ~(cl_command_queue_properties)(CL_QUEUE_OUT_OF_ORDER_EXEC_MODE_ENABLE | CL_QUEUE_PROFILING_ENABLE) ? CL_INVALID_VALUE : CL_SUCCESS;
    cl_command_queue q = e == CL_SUCCESS ? calloc(1, sizeof *q) : NULL;
    if (q) {
        *q = (struct _cl_command_queue){&dispatch, 1, c, props};
        retain(&c->refs);
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
    retain(&q->refs);
    return CL_SUCCESS;
}

static cl_int ReleaseCommandQueue(cl_command_queue q) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (release(&q->refs) == 0) {
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
    // (Its commands waiting too.)
    while (__atomic_load_n(&q->pending, __ATOMIC_ACQUIRE)) {
        drain();
        sched_yield();
    }
    struct request r = {FINISH};
    return call(&r);
}

// ---- Memory ----

static uint64_t base_handle(cl_mem m) { return m->parent ? m->parent->handle : m->handle; }
static size_t base_offset(cl_mem m) { return m->offset; }

// Whether the host may read or write a buffer (CL_MEM_HOST_*).
static int host_reads(cl_mem m) { return !(m->flags & (CL_MEM_HOST_WRITE_ONLY | CL_MEM_HOST_NO_ACCESS)); }
static int host_writes(cl_mem m) { return !(m->flags & (CL_MEM_HOST_READ_ONLY | CL_MEM_HOST_NO_ACCESS)); }

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
            retain(&c->refs);
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
        // Unspecified access and host flags are the parent's.
        const cl_mem_flags access = CL_MEM_READ_WRITE | CL_MEM_WRITE_ONLY | CL_MEM_READ_ONLY;
        const cl_mem_flags host = CL_MEM_USE_HOST_PTR | CL_MEM_ALLOC_HOST_PTR | CL_MEM_COPY_HOST_PTR;
        const cl_mem_flags hostaccess = CL_MEM_HOST_WRITE_ONLY | CL_MEM_HOST_READ_ONLY | CL_MEM_HOST_NO_ACCESS;
        // (A sub-buffer can't give the host access its parent doesn't.)
        if (((flags & CL_MEM_HOST_WRITE_ONLY) && (parent->flags & CL_MEM_HOST_READ_ONLY)) || ((flags & CL_MEM_HOST_READ_ONLY) && (parent->flags & CL_MEM_HOST_WRITE_ONLY)) ||
            ((flags & (CL_MEM_HOST_READ_ONLY | CL_MEM_HOST_WRITE_ONLY)) && (parent->flags & CL_MEM_HOST_NO_ACCESS))) {
            free(m);
            if (err)
                *err = CL_INVALID_VALUE;
            return NULL;
        }
        cl_mem_flags f = flags | (parent->flags & host);
        if (!(flags & access))
            f |= parent->flags & access;
        if (!(flags & hostaccess))
            f |= parent->flags & hostaccess;
        *m = (struct _cl_mem){&dispatch, 1, parent->ctx, f, region->size, region->origin, 0, parent};
        if (parent->host)
            m->host = (char *)parent->host + region->origin;
        retain(&parent->refs);
        retain(&parent->ctx->refs);
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
    retain(&m->refs);
    return CL_SUCCESS;
}

static cl_int ReleaseMemObject(cl_mem m) {
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (release(&m->refs) == 0) {
        for (struct callback *c = m->callbacks, *next; c; c = next) {
            next = c->next;
            c->f(m, c->user);
            free(c);
        }
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

static cl_int SetMemObjectDestructorCallback(cl_mem m, void(CL_CALLBACK *f)(cl_mem, void *), void *user) {
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (!f)
        return CL_INVALID_VALUE;
    struct callback *c = malloc(sizeof *c);
    if (!c)
        return CL_OUT_OF_HOST_MEMORY;
    *c = (struct callback){f, user, m->callbacks};
    m->callbacks = c;
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

// A read or a write: a[0] whether a write, a[1] the offset, a[2] the size,
// a[3] the host memory.
static cl_int run_transfer(struct command *c) {
    if (c->a[0])
        return write_mem(c->mem[0], c->a[1], c->a[2], (const void *)c->a[3]);
    return read_mem(c->mem[0], c->a[1], c->a[2], (void *)c->a[3]);
}

static cl_int EnqueueReadBuffer(cl_command_queue q, cl_mem m, cl_bool blocking, size_t offset, size_t size, void *p, cl_uint nw, const cl_event *wait,
                                cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (!host_reads(m))
        return CL_INVALID_OPERATION;
    if (!p || offset + size > m->size)
        return CL_INVALID_VALUE;
    struct command c = {run_transfer, {0, offset, size, (uintptr_t)p}, .mem = {m}};
    return submit(q, CL_COMMAND_READ_BUFFER, &c, nw, wait, ev, blocking);
}

static cl_int EnqueueWriteBuffer(cl_command_queue q, cl_mem m, cl_bool blocking, size_t offset, size_t size, const void *p, cl_uint nw,
                                 const cl_event *wait, cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (!host_writes(m))
        return CL_INVALID_OPERATION;
    if (!p || offset + size > m->size)
        return CL_INVALID_VALUE;
    struct command c = {run_transfer, {1, offset, size, (uintptr_t)p}, .mem = {m}};
    return submit(q, CL_COMMAND_WRITE_BUFFER, &c, nw, wait, ev, blocking);
}

// a[0] and a[1] the offsets in mem[0] and mem[1], a[2] the size.
static cl_int run_copy(struct command *c) {
    cl_mem src = c->mem[0], dst = c->mem[1];
    struct request r = {COPY, {base_handle(src), base_offset(src) + c->a[0], base_handle(dst), base_offset(dst) + c->a[1], c->a[2]}};
    return call(&r);
}

static cl_int EnqueueCopyBuffer(cl_command_queue q, cl_mem src, cl_mem dst, size_t so, size_t dof, size_t size, cl_uint nw, const cl_event *wait,
                                cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!src || !dst)
        return CL_INVALID_MEM_OBJECT;
    if (so + size > src->size || dof + size > dst->size)
        return CL_INVALID_VALUE;
    struct command c = {run_copy, {so, dof, size}, .mem = {src, dst}};
    return submit(q, CL_COMMAND_COPY_BUFFER, &c, nw, wait, ev, 0);
}

// a[0] the offset, a[1] the pattern's size, a[2] the size; data the
// pattern.
static cl_int run_fill(struct command *c) {
    cl_mem m = c->mem[0];
    struct request r = {FILL, {base_handle(m), base_offset(m) + c->a[0], (uintptr_t)c->data, c->a[1], c->a[2]}};
    return call(&r);
}

static cl_int EnqueueFillBuffer(cl_command_queue q, cl_mem m, const void *pattern, size_t psize, size_t offset, size_t size, cl_uint nw,
                                const cl_event *wait, cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (!pattern || !psize || offset + size > m->size || size % psize || offset % psize)
        return CL_INVALID_VALUE;
    // (The pattern may change once this returns.)
    void *copy = dup_bytes(pattern, psize);
    if (!copy)
        return CL_OUT_OF_HOST_MEMORY;
    struct command c = {run_fill, {offset, psize, size}, copy, {m}};
    return submit(q, CL_COMMAND_FILL_BUFFER, &c, nw, wait, ev, 0);
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

// The three origins and sizes of a rectangle command, copied (data: two
// origins, then the region).
static size_t *rect_copy(const size_t *a, const size_t *b, const size_t *reg) {
    size_t *d = malloc(9 * sizeof *d);
    if (d) {
        memcpy(d, a, 3 * sizeof *d);
        memcpy(d + 3, b, 3 * sizeof *d);
        memcpy(d + 6, reg, 3 * sizeof *d);
    }
    return d;
}

// a[0] whether a write, a[1]-a[4] the pitches, a[5] the host memory.
static cl_int run_rect(struct command *c) {
    size_t *d = c->data;
    return rect(c->mem[0], (int)c->a[0], d, d + 3, d + 6, c->a[1], c->a[2], c->a[3], c->a[4], (void *)c->a[5]);
}

static cl_int rect_command(cl_command_queue q, cl_mem m, int writing, cl_bool blocking, const size_t *bo, const size_t *ho, const size_t *reg, size_t brp,
                           size_t bsp, size_t hrp, size_t hsp, const void *p, cl_uint nw, const cl_event *wait, cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m)
        return CL_INVALID_MEM_OBJECT;
    if (writing ? !host_writes(m) : !host_reads(m))
        return CL_INVALID_OPERATION;
    if (!bo || !ho || !reg || !p || !reg[0] || !reg[1] || !reg[2])
        return CL_INVALID_VALUE;
    // (Checked now, as a command that waits finds out only when it runs.)
    size_t rp = brp ? brp : reg[0], sp = bsp ? bsp : reg[1] * rp;
    if ((bo[2] + reg[2] - 1) * sp + (bo[1] + reg[1] - 1) * rp + bo[0] + reg[0] > m->size)
        return CL_INVALID_VALUE;
    size_t *d = rect_copy(bo, ho, reg);
    if (!d)
        return CL_OUT_OF_HOST_MEMORY;
    struct command c = {run_rect, {writing, brp, bsp, hrp, hsp, (uintptr_t)p}, d, {m}};
    return submit(q, writing ? CL_COMMAND_WRITE_BUFFER_RECT : CL_COMMAND_READ_BUFFER_RECT, &c, nw, wait, ev, blocking);
}

static cl_int EnqueueReadBufferRect(cl_command_queue q, cl_mem m, cl_bool blocking, const size_t *bo, const size_t *ho, const size_t *reg, size_t brp,
                                    size_t bsp, size_t hrp, size_t hsp, void *p, cl_uint nw, const cl_event *wait, cl_event *ev) {
    return rect_command(q, m, 0, blocking, bo, ho, reg, brp, bsp, hrp, hsp, p, nw, wait, ev);
}

static cl_int EnqueueWriteBufferRect(cl_command_queue q, cl_mem m, cl_bool blocking, const size_t *bo, const size_t *ho, const size_t *reg, size_t brp,
                                     size_t bsp, size_t hrp, size_t hsp, const void *p, cl_uint nw, const cl_event *wait, cl_event *ev) {
    return rect_command(q, m, 1, blocking, bo, ho, reg, brp, bsp, hrp, hsp, p, nw, wait, ev);
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
static int mappings_lock;
static void lock_mappings(void) {
    while (__atomic_exchange_n(&mappings_lock, 1, __ATOMIC_ACQUIRE)) {
    }
}
static void unlock_mappings(void) { __atomic_store_n(&mappings_lock, 0, __ATOMIC_RELEASE); }

// a[0]-a[3] the pitches (source row and slice, destination row and slice);
// data the origins and region.
static cl_int run_copy_rect(struct command *c) {
    cl_mem src = c->mem[0], dst = c->mem[1];
    const size_t *so = c->data, *dor = so + 3, *reg = so + 6;
    size_t srp = c->a[0], ssp = c->a[1], drp = c->a[2], dsp = c->a[3];
    // Row by row, through the host.
    char *row = malloc(reg[0]);
    if (!row)
        return CL_OUT_OF_HOST_MEMORY;
    cl_int e = CL_SUCCESS;
    for (size_t z = 0; e == CL_SUCCESS && z < reg[2]; z++)
        for (size_t y = 0; e == CL_SUCCESS && y < reg[1]; y++) {
            size_t s = (so[2] + z) * ssp + (so[1] + y) * srp + so[0];
            size_t d = (dor[2] + z) * dsp + (dor[1] + y) * drp + dor[0];
            if ((e = read_mem(src, s, reg[0], row)) == CL_SUCCESS)
                e = write_mem(dst, d, reg[0], row);
        }
    free(row);
    return e;
}

static cl_int EnqueueCopyBufferRect(cl_command_queue q, cl_mem src, cl_mem dst, const size_t *so, const size_t *dor, const size_t *reg, size_t srp, size_t ssp,
                                    size_t drp, size_t dsp, cl_uint nw, const cl_event *wait, cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!src || !dst)
        return CL_INVALID_MEM_OBJECT;
    if (!so || !dor || !reg || !reg[0] || !reg[1] || !reg[2])
        return CL_INVALID_VALUE;
    if (!srp)
        srp = reg[0];
    if (!ssp)
        ssp = reg[1] * srp;
    if (!drp)
        drp = reg[0];
    if (!dsp)
        dsp = reg[1] * drp;
    if ((so[2] + reg[2] - 1) * ssp + (so[1] + reg[1] - 1) * srp + so[0] + reg[0] > src->size ||
        (dor[2] + reg[2] - 1) * dsp + (dor[1] + reg[1] - 1) * drp + dor[0] + reg[0] > dst->size)
        return CL_INVALID_VALUE;
    size_t *d = rect_copy(so, dor, reg);
    if (!d)
        return CL_OUT_OF_HOST_MEMORY;
    struct command c = {run_copy_rect, {srp, ssp, drp, dsp}, d, {src, dst}};
    return submit(q, CL_COMMAND_COPY_BUFFER_RECT, &c, nw, wait, ev, 0);
}

// The mapping of a buffer at p, unlinked (NULL if there's none).
static struct mapping *unlink_mapping(cl_mem m, void *p) {
    struct mapping *found = NULL;
    lock_mappings();
    for (struct mapping **at = &mappings; *at; at = &(*at)->next) {
        if ((*at)->m == m && (*at)->p == p) {
            found = *at;
            *at = found->next;
            break;
        }
    }
    unlock_mappings();
    return found;
}

// a[0] the offset, a[1] the size, a[2] where it's mapped.
static cl_int run_map(struct command *c) { return read_mem(c->mem[0], c->a[0], c->a[1], (void *)c->a[2]); }

static void *EnqueueMapBuffer(cl_command_queue q, cl_mem m, cl_bool blocking, cl_map_flags flags, size_t offset, size_t size, cl_uint nw,
                              const cl_event *wait, cl_event *ev, cl_int *err) {
    cl_int e = !q ? CL_INVALID_COMMAND_QUEUE : !m ? CL_INVALID_MEM_OBJECT : offset + size > m->size ? CL_INVALID_VALUE : CL_SUCCESS;
    if (e == CL_SUCCESS && (((flags & CL_MAP_READ) && !host_reads(m)) || ((flags & (CL_MAP_WRITE | CL_MAP_WRITE_INVALIDATE_REGION)) && !host_writes(m))))
        e = CL_INVALID_OPERATION;
    if (e == CL_SUCCESS)
        e = waits(q, nw, wait);
    void *p = NULL;
    if (e == CL_SUCCESS) {
        // A buffer made from the host's memory maps to it; others to a copy,
        // written back at unmap (as the host's is too, if mapped to write).
        int write = (flags & (CL_MAP_WRITE | CL_MAP_WRITE_INVALIDATE_REGION)) != 0;
        p = m->host ? (char *)m->host + offset : malloc(size ? size : 1);
        struct mapping *map = !m->host || write ? malloc(sizeof *map) : NULL;
        if (!p || ((!m->host || write) && !map)) {
            if (!m->host)
                free(p);
            free(map);
            e = CL_MAP_FAILURE;
        } else {
            __atomic_add_fetch(&m->maps, 1, __ATOMIC_RELAXED);
            if (map) {
                lock_mappings();
                *map = (struct mapping){p, offset, size, m, write, mappings};
                mappings = map;
                unlock_mappings();
            }
            // The contents, once the work before is done.
            struct command c = {flags & CL_MAP_WRITE_INVALIDATE_REGION ? run_nothing : run_map, {offset, size, (uintptr_t)p}, .mem = {m}};
            e = submit(q, CL_COMMAND_MAP_BUFFER, &c, nw, wait, ev, blocking);
            if (e != CL_SUCCESS) {
                free(unlink_mapping(m, p));
                __atomic_sub_fetch(&m->maps, 1, __ATOMIC_RELAXED);
                if (!m->host)
                    free(p);
            }
        }
    }
    if (err)
        *err = e;
    return e == CL_SUCCESS ? p : NULL;
}

// a[0] the mapped memory: written back (if mapped to write) and unmapped.
static cl_int run_unmap(struct command *c) {
    cl_mem m = c->mem[0];
    void *p = (void *)c->a[0];
    cl_int e = CL_SUCCESS;
    struct mapping *found = unlink_mapping(m, p);
    if (found) {
        if (found->write)
            e = write_mem(m, found->offset, found->size, p);
        if (!m->host)
            free(p);
        free(found);
    }
    __atomic_sub_fetch(&m->maps, 1, __ATOMIC_RELAXED);
    return e;
}

static cl_int EnqueueUnmapMemObject(cl_command_queue q, cl_mem m, void *p, cl_uint nw, const cl_event *wait, cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!m || !m->maps)
        return CL_INVALID_MEM_OBJECT;
    struct command c = {run_unmap, {(uintptr_t)p}, .mem = {m}};
    return submit(q, CL_COMMAND_UNMAP_MEM_OBJECT, &c, nw, wait, ev, 0);
}

// ---- Programs ----

static cl_program program(cl_context c, char *source, size_t len, cl_int *err) {
    cl_program p = source ? calloc(1, sizeof *p) : NULL;
    if (p) {
        *p = (struct _cl_program){&dispatch, 1, c, source, NULL, len, 0, CL_BUILD_NONE};
        retain(&c->refs);
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
// A program's binary: for an executable, its source (built again when
// loaded); for a compiled object or a library, a tag and its bitcode.
static const char object_tag[] = "WENVO", library_tag[] = "WENVL", executable_tag[] = "WENVX";

static cl_program CreateProgramWithBinary(cl_context c, cl_uint n, const cl_device_id *devices, const size_t *lengths, const unsigned char **binaries,
                                          cl_int *status, cl_int *err) {
    if (!c || n != 1 || !devices || devices[0] != &device || !lengths || !binaries || !binaries[0] || !lengths[0]) {
        if (err)
            *err = !c ? CL_INVALID_CONTEXT : CL_INVALID_VALUE;
        return NULL;
    }
    if (status)
        status[0] = CL_SUCCESS;
    const char *b = (const char *)binaries[0];
    int tagged = lengths[0] > 5;
    int kind = tagged && !memcmp(b, library_tag, 5) ? 1 : tagged && !memcmp(b, executable_tag, 5) ? 2 : tagged && !memcmp(b, object_tag, 5) ? 0 : -1;
    if (kind >= 0) {
        cl_program p = program(c, dup_bytes("", 0), 0, err);
        if (p) {
            struct request r = {LOAD, {(uintptr_t)(b + 5), lengths[0] - 5, (uint64_t)kind}};
            cl_int e = call(&r);
            p->handle = r.r[0];
            p->binary = kind == 1 ? CL_PROGRAM_BINARY_TYPE_LIBRARY : kind == 2 ? CL_PROGRAM_BINARY_TYPE_EXECUTABLE : CL_PROGRAM_BINARY_TYPE_COMPILED_OBJECT;
            p->prebuilt = kind == 2;
            if (e != CL_SUCCESS && err)
                *err = e;
        }
        return p;
    }
    return program(c, dup_bytes(b, lengths[0]), lengths[0], err);
}

// An object's or library's bitcode, tagged (a new buffer; its length).
static char *object_binary(cl_program p, size_t *len) {
    struct request r = {BINARY, {p->handle, 0, 0}};
    if (call(&r) != CL_SUCCESS)
        return NULL;
    char *b = malloc(r.r[0] + 5);
    if (!b)
        return NULL;
    memcpy(b, r.r[2] == 1 ? library_tag : r.r[2] == 2 ? executable_tag : object_tag, 5);
    r = (struct request){BINARY, {p->handle, (uintptr_t)(b + 5), r.r[0]}};
    call(&r);
    *len = r.r[0] + 5;
    return b;
}

static cl_int RetainProgram(cl_program p) {
    if (!p)
        return CL_INVALID_PROGRAM;
    retain(&p->refs);
    return CL_SUCCESS;
}

static cl_int ReleaseProgram(cl_program p) {
    if (!p)
        return CL_INVALID_PROGRAM;
    if (release(&p->refs) == 0) {
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
    if (p->prebuilt) {
        p->status = CL_BUILD_SUCCESS;
        if (notify)
            notify(p, user);
        return CL_SUCCESS;
    }
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
    p->binary = e == CL_SUCCESS ? CL_PROGRAM_BINARY_TYPE_EXECUTABLE : CL_PROGRAM_BINARY_TYPE_NONE;
    if (notify)
        notify(p, user);
    return e;
}

static cl_int CompileProgram(cl_program p, cl_uint n, const cl_device_id *devices, const char *options, cl_uint nh, const cl_program *headers,
                             const char **names, void(CL_CALLBACK *notify)(cl_program, void *), void *user) {
    if (!p)
        return CL_INVALID_PROGRAM;
    if ((n && !devices) || (!n && devices) || (nh && (!headers || !names)) || (!nh && (headers || names)))
        return CL_INVALID_VALUE;
    for (cl_uint i = 0; i < n; i++)
        if (devices[i] != &device)
            return CL_INVALID_DEVICE;
    // The headers it includes by name: (source, length, name, length) each.
    uint64_t *records = calloc(nh + 1, 4 * sizeof *records);
    if (!records)
        return CL_OUT_OF_HOST_MEMORY;
    for (cl_uint i = 0; i < nh; i++) {
        records[i * 4] = (uintptr_t)headers[i]->source;
        records[i * 4 + 1] = headers[i]->len;
        records[i * 4 + 2] = (uintptr_t)names[i];
        records[i * 4 + 3] = strlen(names[i]);
    }
    if (p->handle) {
        struct request f = {FREE, {p->handle}};
        call(&f);
        p->handle = 0;
    }
    free(p->options);
    p->options = dup_bytes(options ? options : "", options ? strlen(options) : 0);
    struct request r = {COMPILE, {(uintptr_t)p->source, p->len, (uintptr_t)p->options, strlen(p->options), (uintptr_t)records, nh}};
    cl_int e = call(&r);
    free(records);
    p->handle = r.r[0];
    p->status = e == CL_SUCCESS ? CL_BUILD_SUCCESS : CL_BUILD_ERROR;
    p->binary = e == CL_SUCCESS ? CL_PROGRAM_BINARY_TYPE_COMPILED_OBJECT : CL_PROGRAM_BINARY_TYPE_NONE;
    if (notify)
        notify(p, user);
    return e;
}

static cl_program LinkProgram(cl_context c, cl_uint n, const cl_device_id *devices, const char *options, cl_uint np, const cl_program *programs,
                              void(CL_CALLBACK *notify)(cl_program, void *), void *user, cl_int *err) {
    cl_int e = !c ? CL_INVALID_CONTEXT : (!np || !programs || (n && !devices) || (!n && devices) || (!notify && user)) ? CL_INVALID_VALUE : CL_SUCCESS;
    for (cl_uint i = 0; e == CL_SUCCESS && i < n; i++)
        if (devices[i] != &device)
            e = CL_INVALID_DEVICE;
    for (cl_uint i = 0; e == CL_SUCCESS && i < np; i++)
        if (!programs[i] || (programs[i]->binary != CL_PROGRAM_BINARY_TYPE_COMPILED_OBJECT && programs[i]->binary != CL_PROGRAM_BINARY_TYPE_LIBRARY))
            e = CL_INVALID_PROGRAM;
    uint64_t *handles = e == CL_SUCCESS ? malloc(np * sizeof *handles) : NULL;
    if (e == CL_SUCCESS && !handles)
        e = CL_OUT_OF_HOST_MEMORY;
    cl_program p = e == CL_SUCCESS ? program(c, dup_bytes("", 0), 0, &e) : NULL;
    if (p) {
        for (cl_uint i = 0; i < np; i++)
            handles[i] = programs[i]->handle;
        p->options = dup_bytes(options ? options : "", options ? strlen(options) : 0);
        struct request r = {LINK, {(uintptr_t)handles, np, (uintptr_t)p->options, strlen(p->options)}};
        e = call(&r);
        p->handle = r.r[0];
        p->status = e == CL_SUCCESS ? CL_BUILD_SUCCESS : CL_BUILD_ERROR;
        int library = strstr(p->options, "-create-library") != NULL;
        p->binary = e != CL_SUCCESS ? CL_PROGRAM_BINARY_TYPE_NONE : library ? CL_PROGRAM_BINARY_TYPE_LIBRARY : CL_PROGRAM_BINARY_TYPE_EXECUTABLE;
        p->prebuilt = e == CL_SUCCESS && !library;
        if (notify)
            notify(p, user);
    }
    free(handles);
    if (err)
        *err = e;
    // (A failed link still returns the program, with its log.)
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
    case CL_PROGRAM_BINARIES: {
        size_t len = p->len;
        char *bits = p->source;
        int object = p->binary == CL_PROGRAM_BINARY_TYPE_COMPILED_OBJECT || p->binary == CL_PROGRAM_BINARY_TYPE_LIBRARY || p->prebuilt;
        if (object && !(bits = object_binary(p, &len)))
            return CL_OUT_OF_RESOURCES;
        if (what == CL_PROGRAM_BINARY_SIZES) {
            cl_int e = info(&len, sizeof len, cap, out, size_ret);
            if (object)
                free(bits);
            return e;
        }
        if (out) {
            if (cap < sizeof(char *))
                return CL_INVALID_VALUE;
            unsigned char *b = *(unsigned char **)out;
            if (b)
                memcpy(b, bits, len);
        }
        if (object)
            free(bits);
        if (size_ret)
            *size_ret = sizeof(char *);
        return CL_SUCCESS;
    }
    case CL_PROGRAM_NUM_KERNELS:
    case CL_PROGRAM_KERNEL_NAMES: {
        if (p->status != CL_BUILD_SUCCESS || p->binary != CL_PROGRAM_BINARY_TYPE_EXECUTABLE)
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
        INFO_OF(cl_program_binary_type, p->binary);
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
    cl_int e = !p ? CL_INVALID_PROGRAM : p->status != CL_BUILD_SUCCESS || p->binary != CL_PROGRAM_BINARY_TYPE_EXECUTABLE ? CL_INVALID_PROGRAM_EXECUTABLE : !name ? CL_INVALID_VALUE : CL_SUCCESS;
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
                free(k->name), free(k->args), free(k->block), free(k->handles), free(k->locals), free(k->set), free(k->attributes), free(k);
                k = NULL;
            } else {
                retain(&p->refs);
                retain(&p->kernels);
                struct request q = {ATTRIBUTES, {p->handle, (uintptr_t)name, strlen(name), 0, 0}};
                if (call(&q) == CL_SUCCESS)
                    k->static_local = q.r[2];
                if (call(&q) == CL_SUCCESS && (k->attributes = calloc(1, q.r[0] + 1))) {
                    q = (struct request){ATTRIBUTES, {p->handle, (uintptr_t)name, strlen(name), (uintptr_t)k->attributes, q.r[0]}};
                    call(&q);
                }
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
    retain(&k->refs);
    return CL_SUCCESS;
}

static cl_int ReleaseKernel(cl_kernel k) {
    if (!k)
        return CL_INVALID_KERNEL;
    if (release(&k->refs) == 0) {
        release(&k->program->kernels);
        ReleaseProgram(k->program);
        free(k->name), free(k->args), free(k->block), free(k->handles), free(k->locals), free(k->set), free(k->attributes), free(k);
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
        // A null buffer argument (allowed: the kernel may test for it, not
        // use it): 0xffffffff, which the kernel sees as a null pointer.
        k->handles[i] = m ? base_handle(m) : 0;
        uint32_t offset = m ? (uint32_t)base_offset(m) : 0xffffffffu;
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
        INFO_STR(k->attributes ? k->attributes : "");
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
    case CL_KERNEL_LOCAL_MEM_SIZE: {
        cl_ulong total = k->static_local;
        for (cl_uint i = 0; i < k->nargs; i++)
            total += k->locals[i];
        INFO_OF(cl_ulong, total);
    }
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
        INFO_OF(cl_kernel_arg_access_qualifier, (a->qualifiers >> 16) ? (a->qualifiers >> 16) : CL_KERNEL_ARG_ACCESS_NONE);
    case CL_KERNEL_ARG_TYPE_NAME:
        INFO_STR(a->type);
    case CL_KERNEL_ARG_TYPE_QUALIFIER:
        INFO_OF(cl_kernel_arg_type_qualifier, a->qualifiers & 0xffff);
    case CL_KERNEL_ARG_NAME:
        if (!a->name[0])
            return CL_KERNEL_ARG_INFO_NOT_AVAILABLE;
        INFO_STR(a->name);
    }
    return CL_INVALID_VALUE;
}

// a[0] the dimensions; data the arguments (see EnqueueNDRangeKernel).
static cl_int run_kernel(struct command *c) {
    cl_kernel k = c->kernel;
    uint8_t *sizes = c->data, *handles = sizes + 9 * sizeof(uint32_t);
    uint8_t *locals = handles + k->nargs * sizeof *k->handles, *block = locals + k->nargs * sizeof *k->locals;
    struct request r = {RUN,
                        {k->program->handle, (uintptr_t)k->name, strlen(k->name), c->a[0], (uintptr_t)sizes, (uintptr_t)block, k->block_size,
                         (uintptr_t)handles, (uintptr_t)locals}};
    return call(&r);
}

static cl_int EnqueueNDRangeKernel(cl_command_queue q, cl_kernel k, cl_uint dims, const size_t *offset, const size_t *global, const size_t *local,
                                   cl_uint nw, const cl_event *wait, cl_event *ev) {
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
    // Its arguments as they are now (they may be set again once this
    // returns): the sizes, the block, the buffers' handles and the local
    // memory sizes, one after another.
    size_t n = sizeof sizes + k->block_size + k->nargs * (sizeof *k->handles + sizeof *k->locals);
    uint8_t *d = malloc(n);
    if (!d)
        return CL_OUT_OF_HOST_MEMORY;
    uint8_t *at = d;
    memcpy(at, sizes, sizeof sizes);
    at += sizeof sizes;
    memcpy(at, k->handles, k->nargs * sizeof *k->handles);
    at += k->nargs * sizeof *k->handles;
    memcpy(at, k->locals, k->nargs * sizeof *k->locals);
    at += k->nargs * sizeof *k->locals;
    memcpy(at, k->block, k->block_size);
    struct command c = {run_kernel, {dims}, d, .kernel = k};
    return submit(q, CL_COMMAND_NDRANGE_KERNEL, &c, nw, wait, ev, 0);
}

static cl_int EnqueueTask(cl_command_queue q, cl_kernel k, cl_uint nw, const cl_event *wait, cl_event *ev) {
    size_t one = 1;
    return EnqueueNDRangeKernel(q, k, 1, NULL, &one, &one, nw, wait, ev);
}

static cl_int EnqueueMigrateMemObjects(cl_command_queue q, cl_uint n, const cl_mem *objects, cl_mem_migration_flags flags, cl_uint nw, const cl_event *wait,
                                       cl_event *ev) {
    // One device: nothing to move.
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!n || !objects)
        return CL_INVALID_VALUE;
    for (cl_uint i = 0; i < n; i++)
        if (!objects[i])
            return CL_INVALID_MEM_OBJECT;
    if (flags & ~(cl_mem_migration_flags)(CL_MIGRATE_MEM_OBJECT_HOST | CL_MIGRATE_MEM_OBJECT_CONTENT_UNDEFINED))
        return CL_INVALID_VALUE;
    struct command c = {run_nothing};
    return submit(q, CL_COMMAND_MIGRATE_MEM_OBJECTS, &c, nw, wait, ev, 0);
}

// ---- Events and synchronization ----

// (Queues are in order: a marker or a barrier waits for the commands
// before it, and its wait list.)
static cl_int EnqueueMarkerWithWaitList(cl_command_queue q, cl_uint nw, const cl_event *wait, cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    struct command c = {run_nothing};
    return submit(q, CL_COMMAND_MARKER, &c, nw, wait, ev, 0);
}

static cl_int EnqueueBarrierWithWaitList(cl_command_queue q, cl_uint nw, const cl_event *wait, cl_event *ev) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    struct command c = {run_nothing};
    return submit(q, CL_COMMAND_BARRIER, &c, nw, wait, ev, 0);
}

static cl_int EnqueueMarker(cl_command_queue q, cl_event *ev) { return EnqueueMarkerWithWaitList(q, 0, NULL, ev); }
static cl_int EnqueueBarrier(cl_command_queue q) { return q ? CL_SUCCESS : CL_INVALID_COMMAND_QUEUE; }
static cl_int EnqueueWaitForEvents(cl_command_queue q, cl_uint n, const cl_event *events) {
    if (!q)
        return CL_INVALID_COMMAND_QUEUE;
    if (!n || !events)
        return CL_INVALID_VALUE;
    return EnqueueBarrierWithWaitList(q, n, events, NULL);
}

static cl_int WaitForEvents(cl_uint n, const cl_event *events) {
    if (!n || !events)
        return CL_INVALID_VALUE;
    for (cl_uint i = 0; i < n; i++)
        if (!events[i])
            return CL_INVALID_EVENT;
    cl_int e = CL_SUCCESS;
    for (cl_uint i = 0; i < n; i++)
        if (wait_for(events[i]) != CL_SUCCESS)
            e = CL_EXEC_STATUS_ERROR_FOR_EVENTS_IN_WAIT_LIST;
    if (e != CL_SUCCESS)
        return e;
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
        INFO_OF(cl_int, status_of(e));
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
    // Commands run as they're enqueued: queued, submitted and started
    // together.
    switch (what) {
    case CL_PROFILING_COMMAND_QUEUED:
    case CL_PROFILING_COMMAND_SUBMIT:
    case CL_PROFILING_COMMAND_START:
        INFO_OF(cl_ulong, e->began);
    case CL_PROFILING_COMMAND_END:
    case CL_PROFILING_COMMAND_COMPLETE:
        INFO_OF(cl_ulong, e->ended);
    }
    return CL_INVALID_VALUE;
}

static cl_event CreateUserEvent(cl_context c, cl_int *err) {
    cl_event e = c ? event(NULL, CL_COMMAND_USER) : NULL;
    if (e) {
        e->ctx = c;
        e->status = CL_SUBMITTED;
        retain(&c->refs);
    }
    if (err)
        *err = !c ? CL_INVALID_CONTEXT : e ? CL_SUCCESS : CL_OUT_OF_HOST_MEMORY;
    return e;
}

static cl_int SetUserEventStatus(cl_event e, cl_int status) {
    if (!e || e->type != CL_COMMAND_USER)
        return CL_INVALID_EVENT;
    if (status > CL_COMPLETE)
        return CL_INVALID_VALUE;
    if (status_of(e) <= CL_COMPLETE)
        return CL_INVALID_OPERATION;
    set_status(e, status);
    // The commands that waited for it.
    drain();
    return CL_SUCCESS;
}

static cl_int SetEventCallback(cl_event e, cl_int type, void(CL_CALLBACK *notify)(cl_event, cl_int, void *), void *user) {
    if (!e)
        return CL_INVALID_EVENT;
    if (!notify)
        return CL_INVALID_VALUE;
    if (type != CL_COMPLETE && type != CL_RUNNING && type != CL_SUBMITTED)
        return CL_INVALID_VALUE;
    struct waiting *w = malloc(sizeof *w);
    if (!w)
        return CL_OUT_OF_HOST_MEMORY;
    // Now, if the event is there already; else when it gets there
    // (`set_status`).
    lock_commands();
    cl_int s = e->status;
    if (s > type) {
        *w = (struct waiting){notify, user, type, e->waiting};
        e->waiting = w;
    }
    unlock_commands();
    if (s <= type) {
        free(w);
        notify(e, s, user);
    }
    return CL_SUCCESS;
}

static cl_int RetainEvent(cl_event e) {
    if (!e)
        return CL_INVALID_EVENT;
    retain(&e->refs);
    return CL_SUCCESS;
}

static cl_int ReleaseEvent(cl_event e) {
    if (!e)
        return CL_INVALID_EVENT;
    if (release(&e->refs) == 0) {
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
        .clSetMemObjectDestructorCallback = SetMemObjectDestructorCallback,
        .clSetEventCallback = SetEventCallback,
        .clCreateUserEvent = CreateUserEvent,
        .clSetUserEventStatus = SetUserEventStatus,
        .clEnqueueReadBufferRect = EnqueueReadBufferRect,
        .clEnqueueWriteBufferRect = EnqueueWriteBufferRect,
        .clEnqueueCopyBufferRect = EnqueueCopyBufferRect,
        .clRetainDevice = RetainDevice,
        .clReleaseDevice = ReleaseDevice,
        .clCompileProgram = CompileProgram,
        .clLinkProgram = LinkProgram,
        .clUnloadPlatformCompiler = UnloadPlatformCompiler,
        .clGetKernelArgInfo = GetKernelArgInfo,
        .clEnqueueFillBuffer = EnqueueFillBuffer,
        .clEnqueueMigrateMemObjects = EnqueueMigrateMemObjects,
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
