// wasm-env's Vulkan driver: an installable client driver (ICD) for the
// Khronos loader (libvulkan.so.1), implementing a portable subset of
// Vulkan 1.0 on the runtime's GPU service through /dev/wasm-env-gpu
// (runtime/src/gpu.rs has the requests, gpu/src/vulkan.rs the shaders and
// the commands). Compute first: memory, buffers, SPIR-V shader modules,
// descriptor sets of buffers, compute pipelines, push constants, command
// buffers (recorded as a stream of commands the runtime replays) and
// fences. One physical device, with one queue.
//
// Device memory is a buffer in the runtime. Memory the program maps is a
// copy in its own memory: sent to the device before each submission, and
// after a wait (a fence, the queue, the device) updated where the device
// may have written (what HOST_COHERENT promises; the program's own writes
// elsewhere in it stay). A submission while device writes to mapped memory
// haven't been read back reads them first, so it doesn't overwrite them.
//
// Like the OpenCL driver, it uses nothing from the C library but
// allocation, a few string functions, open, ioctl and close: the same
// library serves glibc and musl programs.
#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>
#include <vulkan/vk_icd.h>
#include <stddef.h>
#include <stdint.h>

void *malloc(size_t);
void *calloc(size_t, size_t);
void *realloc(void *, size_t);
void free(void *);
void *memcpy(void *, const void *, size_t);
void *memset(void *, int, size_t);
size_t strlen(const char *);
int strcmp(const char *, const char *);
int snprintf(char *, size_t, const char *, ...);
int open(const char *, int, ...);
int ioctl(int, unsigned long, ...);
#define O_RDWR 02
#define O_CLOEXEC 02000000

#define API __attribute__((visibility("default")))
#define GPU_CALL 0xc0805740u
#define CALL VKAPI_ATTR
#define CC VKAPI_CALL

enum { INFO = 1, CREATE, FREE, WRITE, READ, FINISH = 13, VK_SHADER = 20, VK_PIPELINE, VK_SUBMIT, VK_LIMITS };
// The command stream's operations (gpu/src/vulkan.rs, `decode`).
enum { BIND_PIPELINE = 1, BIND_SET, PUSH, DISPATCH, DISPATCH_INDIRECT, COPY, FILL, UPDATE };

struct request {
    uint64_t op, a[9], r[6];
};

// The device, opened when a program first asks for physical devices.
static int fd = -1;
static char device_name[64];
// VK_LIMITS's words (runtime/src/gpu.rs).
enum {
    L_STORAGE_ALIGN,
    L_UNIFORM_ALIGN,
    L_STORAGE_RANGE,
    L_UNIFORM_RANGE,
    L_GROUPS,
    L_SIZE_X,
    L_SIZE_Y,
    L_SIZE_Z,
    L_INVOCATIONS,
    L_SHARED,
    L_SETS,
    L_STORAGE_BUFFERS,
    L_UNIFORM_BUFFERS,
    L_ALLOCATION,
    L_MEMORY,
    L_PUSH,
    L_VENDOR,
    L_DEVICE,
    L_TYPE,
    L_COUNT
};
static uint64_t limits[L_COUNT];

static int call(struct request *q) {
    return ioctl(fd, GPU_CALL, q) == 0 && q->r[1] == 0 ? 0 : -1;
}

static int open_device(void) {
    if (fd >= 0)
        return 0;
    int f = open("/dev/wasm-env-gpu", O_RDWR | O_CLOEXEC);
    if (f < 0)
        return -1;
    fd = f;
    char info[256];
    struct request q = {INFO, {(uint64_t)(uintptr_t)info, sizeof info}};
    struct request l = {VK_LIMITS, {(uint64_t)(uintptr_t)limits, sizeof limits}};
    if (call(&q) || call(&l))
        return -1;
    memcpy(device_name, info, 63);
    return 0;
}

// ---- Objects ----

struct VkInstance_T {
    VK_LOADER_DATA loader;
};
struct VkPhysicalDevice_T {
    VK_LOADER_DATA loader;
};
static struct VkPhysicalDevice_T physical = {{ICD_LOADER_MAGIC}};

struct VkDeviceMemory_T {
    uint64_t handle;
    VkDeviceSize size;
    // The program's copy, while it's mapped (or NULL).
    uint8_t *map;
    // Ranges the device may have written since the copy was last updated
    // (offset, end pairs).
    VkDeviceSize *pending;
    size_t npending, cap;
    struct VkDeviceMemory_T *next, *prev;
};
struct VkQueue_T {
    VK_LOADER_DATA loader;
};
struct VkDevice_T {
    VK_LOADER_DATA loader;
    struct VkQueue_T queue;
    // Memory mapped now (a list).
    struct VkDeviceMemory_T *mapped;
};
struct VkBuffer_T {
    VkDeviceSize size;
    VkBufferUsageFlags usage;
    VkDeviceMemory memory;
    VkDeviceSize offset;
};
struct VkShaderModule_T {
    uint64_t handle;
};
struct VkPipeline_T {
    uint64_t handle;
};
struct VkPipelineLayout_T {
    uint32_t sets;
    VkDescriptorSetLayout layouts[8];
};
struct VkDescriptorSetLayout_T {
    uint32_t n;
    VkDescriptorSetLayoutBinding *bindings;
};
// A descriptor: a buffer's range (its memory's handle and the range's
// place in it).
struct descriptor {
    VkDescriptorType type;
    VkBuffer buffer;
    VkDeviceSize offset, range;
};
struct VkDescriptorSet_T {
    VkDescriptorSetLayout layout;
    struct descriptor *d; // per binding of the layout, in its order
    VkDescriptorPool pool;
    struct VkDescriptorSet_T *next;
};
struct VkDescriptorPool_T {
    struct VkDescriptorSet_T *sets;
};
struct VkCommandPool_T {
    int unused;
};
// Device memory a recorded command may write: read back after a wait.
struct write {
    VkDeviceMemory memory;
    VkDeviceSize offset, end;
};
struct VkCommandBuffer_T {
    VK_LOADER_DATA loader;
    uint8_t *s;
    size_t n, cap;
    struct write *writes;
    size_t nwrites, wcap;
    // Bound now: the compute pipeline's layout (for dynamic offsets).
    int failed;
};
struct VkFence_T {
    int signaled;
};
struct VkSemaphore_T {
    int unused;
};
struct VkEvent_T {
    int set;
};
struct VkPipelineCache_T {
    int unused;
};

static void *zalloc(size_t n) {
    return calloc(1, n ? n : 1);
}

// ---- Instance and physical device ----

static CALL VkResult CC CreateInstance(const VkInstanceCreateInfo *info, const VkAllocationCallbacks *a, VkInstance *out) {
    for (uint32_t i = 0; i < info->enabledExtensionCount; i++)
        if (strcmp(info->ppEnabledExtensionNames[i], VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME))
            return VK_ERROR_EXTENSION_NOT_PRESENT;
    struct VkInstance_T *i = zalloc(sizeof *i);
    if (!i)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    set_loader_magic_value(i);
    *out = i;
    return VK_SUCCESS;
}

static CALL void CC DestroyInstance(VkInstance i, const VkAllocationCallbacks *a) {
    free(i);
}

// Copies `n` of `items` (each `size` bytes) to `out` as Vulkan's
// enumerations do: the count alone, or as many as fit (VK_INCOMPLETE).
static VkResult enumerate(uint32_t *count, void *out, const void *items, uint32_t n, size_t size) {
    if (!out) {
        *count = n;
        return VK_SUCCESS;
    }
    uint32_t k = *count < n ? *count : n;
    memcpy(out, items, k * size);
    *count = k;
    return k < n ? VK_INCOMPLETE : VK_SUCCESS;
}

static const VkExtensionProperties instance_extensions[] = {
    {VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME, VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_SPEC_VERSION},
};

static CALL VkResult CC EnumerateInstanceExtensionProperties(const char *layer, uint32_t *count, VkExtensionProperties *out) {
    if (layer)
        return VK_ERROR_LAYER_NOT_PRESENT;
    return enumerate(count, out, instance_extensions, 1, sizeof *out);
}

static CALL VkResult CC EnumerateInstanceVersion(uint32_t *version) {
    *version = VK_API_VERSION_1_0;
    return VK_SUCCESS;
}

static CALL VkResult CC EnumeratePhysicalDevices(VkInstance i, uint32_t *count, VkPhysicalDevice *out) {
    if (open_device()) {
        *count = 0;
        return VK_SUCCESS;
    }
    VkPhysicalDevice p = &physical;
    return enumerate(count, out, &p, 1, sizeof p);
}

static CALL void CC GetPhysicalDeviceProperties(VkPhysicalDevice pd, VkPhysicalDeviceProperties *p) {
    memset(p, 0, sizeof *p);
    p->apiVersion = VK_API_VERSION_1_0;
    p->driverVersion = VK_MAKE_VERSION(0, 1, 0);
    p->vendorID = (uint32_t)limits[L_VENDOR];
    p->deviceID = (uint32_t)limits[L_DEVICE];
    p->deviceType = (VkPhysicalDeviceType)limits[L_TYPE];
    snprintf(p->deviceName, sizeof p->deviceName, "%s (wasm-env)", device_name);
    memcpy(p->pipelineCacheUUID, "wasm-env-vulkan", 16);
    VkPhysicalDeviceLimits *l = &p->limits;
    // Images and graphics: Vulkan's minimums (not supported yet).
    l->maxImageDimension1D = l->maxImageDimension2D = l->maxImageDimensionCube = 4096;
    l->maxImageDimension3D = 256;
    l->maxImageArrayLayers = 256;
    l->maxTexelBufferElements = 65536;
    l->maxUniformBufferRange = (uint32_t)(limits[L_UNIFORM_RANGE] < 0xffffffffu ? limits[L_UNIFORM_RANGE] : 0xffffffffu);
    l->maxStorageBufferRange = (uint32_t)(limits[L_STORAGE_RANGE] < 0xffffffffu ? limits[L_STORAGE_RANGE] : 0xffffffffu);
    l->maxPushConstantsSize = (uint32_t)limits[L_PUSH];
    l->maxMemoryAllocationCount = 4096;
    l->maxSamplerAllocationCount = 4000;
    l->bufferImageGranularity = 1;
    l->maxBoundDescriptorSets = (uint32_t)limits[L_SETS];
    l->maxPerStageDescriptorUniformBuffers = (uint32_t)limits[L_UNIFORM_BUFFERS];
    l->maxPerStageDescriptorStorageBuffers = (uint32_t)limits[L_STORAGE_BUFFERS];
    l->maxPerStageResources = (uint32_t)(limits[L_UNIFORM_BUFFERS] + limits[L_STORAGE_BUFFERS]);
    l->maxDescriptorSetUniformBuffers = (uint32_t)limits[L_UNIFORM_BUFFERS];
    l->maxDescriptorSetUniformBuffersDynamic = 8;
    l->maxDescriptorSetStorageBuffers = (uint32_t)limits[L_STORAGE_BUFFERS];
    l->maxDescriptorSetStorageBuffersDynamic = 4;
    l->maxComputeSharedMemorySize = (uint32_t)limits[L_SHARED];
    for (int k = 0; k < 3; k++)
        l->maxComputeWorkGroupCount[k] = (uint32_t)limits[L_GROUPS];
    l->maxComputeWorkGroupInvocations = (uint32_t)limits[L_INVOCATIONS];
    l->maxComputeWorkGroupSize[0] = (uint32_t)limits[L_SIZE_X];
    l->maxComputeWorkGroupSize[1] = (uint32_t)limits[L_SIZE_Y];
    l->maxComputeWorkGroupSize[2] = (uint32_t)limits[L_SIZE_Z];
    l->minMemoryMapAlignment = 64;
    l->minTexelBufferOffsetAlignment = 256;
    l->minUniformBufferOffsetAlignment = limits[L_UNIFORM_ALIGN];
    l->minStorageBufferOffsetAlignment = limits[L_STORAGE_ALIGN];
    l->maxViewports = 1;
    l->maxViewportDimensions[0] = l->maxViewportDimensions[1] = 4096;
    l->viewportBoundsRange[0] = -8192;
    l->viewportBoundsRange[1] = 8191;
    l->maxFramebufferWidth = l->maxFramebufferHeight = 4096;
    l->maxFramebufferLayers = 256;
    l->maxColorAttachments = 4;
    l->maxSamplerAnisotropy = 1;
    l->timestampComputeAndGraphics = VK_FALSE;
    l->timestampPeriod = 1;
    l->optimalBufferCopyOffsetAlignment = 4;
    l->optimalBufferCopyRowPitchAlignment = 256;
    l->nonCoherentAtomSize = 4;
}

static CALL void CC GetPhysicalDeviceFeatures(VkPhysicalDevice pd, VkPhysicalDeviceFeatures *f) {
    memset(f, 0, sizeof *f);
}

static const VkQueueFamilyProperties family = {VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT, 1, 0, {1, 1, 1}};

static CALL void CC GetPhysicalDeviceQueueFamilyProperties(VkPhysicalDevice pd, uint32_t *count, VkQueueFamilyProperties *out) {
    enumerate(count, out, &family, 1, sizeof family);
}

// One heap, one memory type: the device's, which the program may map.
static CALL void CC GetPhysicalDeviceMemoryProperties(VkPhysicalDevice pd, VkPhysicalDeviceMemoryProperties *m) {
    memset(m, 0, sizeof *m);
    m->memoryTypeCount = 1;
    m->memoryTypes[0].propertyFlags = VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT | VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    m->memoryHeapCount = 1;
    m->memoryHeaps[0].size = limits[L_MEMORY];
    m->memoryHeaps[0].flags = VK_MEMORY_HEAP_DEVICE_LOCAL_BIT;
}

static CALL void CC GetPhysicalDeviceFormatProperties(VkPhysicalDevice pd, VkFormat f, VkFormatProperties *p) {
    memset(p, 0, sizeof *p);
}

static CALL VkResult CC GetPhysicalDeviceImageFormatProperties(VkPhysicalDevice pd, VkFormat f, VkImageType t, VkImageTiling tl, VkImageUsageFlags u, VkImageCreateFlags c, VkImageFormatProperties *p) {
    return VK_ERROR_FORMAT_NOT_SUPPORTED;
}

static CALL void CC GetPhysicalDeviceSparseImageFormatProperties(VkPhysicalDevice pd, VkFormat f, VkImageType t, VkSampleCountFlagBits s, VkImageUsageFlags u, VkImageTiling tl, uint32_t *count, VkSparseImageFormatProperties *p) {
    *count = 0;
}

// The ...2 forms (VK_KHR_get_physical_device_properties2): the 1.0 ones,
// and nothing in their chains.
static CALL void CC GetPhysicalDeviceProperties2(VkPhysicalDevice pd, VkPhysicalDeviceProperties2 *p) {
    GetPhysicalDeviceProperties(pd, &p->properties);
}
static CALL void CC GetPhysicalDeviceFeatures2(VkPhysicalDevice pd, VkPhysicalDeviceFeatures2 *f) {
    GetPhysicalDeviceFeatures(pd, &f->features);
}
static CALL void CC GetPhysicalDeviceMemoryProperties2(VkPhysicalDevice pd, VkPhysicalDeviceMemoryProperties2 *m) {
    GetPhysicalDeviceMemoryProperties(pd, &m->memoryProperties);
}
static CALL void CC GetPhysicalDeviceFormatProperties2(VkPhysicalDevice pd, VkFormat f, VkFormatProperties2 *p) {
    GetPhysicalDeviceFormatProperties(pd, f, &p->formatProperties);
}
static CALL void CC GetPhysicalDeviceQueueFamilyProperties2(VkPhysicalDevice pd, uint32_t *count, VkQueueFamilyProperties2 *out) {
    if (!out) {
        *count = 1;
        return;
    }
    if (*count >= 1)
        out[0].queueFamilyProperties = family;
    *count = *count < 1 ? *count : 1;
}
static CALL VkResult CC GetPhysicalDeviceImageFormatProperties2(VkPhysicalDevice pd, const VkPhysicalDeviceImageFormatInfo2 *i, VkImageFormatProperties2 *p) {
    return VK_ERROR_FORMAT_NOT_SUPPORTED;
}
static CALL void CC GetPhysicalDeviceSparseImageFormatProperties2(VkPhysicalDevice pd, const VkPhysicalDeviceSparseImageFormatInfo2 *i, uint32_t *count, VkSparseImageFormatProperties2 *p) {
    *count = 0;
}

static CALL VkResult CC EnumerateDeviceExtensionProperties(VkPhysicalDevice pd, const char *layer, uint32_t *count, VkExtensionProperties *out) {
    *count = 0;
    return layer ? VK_ERROR_LAYER_NOT_PRESENT : VK_SUCCESS;
}

static CALL VkResult CC EnumerateDeviceLayerProperties(VkPhysicalDevice pd, uint32_t *count, VkLayerProperties *out) {
    *count = 0;
    return VK_SUCCESS;
}

// ---- Device and queue ----

static CALL VkResult CC CreateDevice(VkPhysicalDevice pd, const VkDeviceCreateInfo *info, const VkAllocationCallbacks *a, VkDevice *out) {
    if (info->enabledExtensionCount)
        return VK_ERROR_EXTENSION_NOT_PRESENT;
    for (uint32_t i = 0; i < info->queueCreateInfoCount; i++)
        if (info->pQueueCreateInfos[i].queueFamilyIndex != 0 || info->pQueueCreateInfos[i].queueCount > 1)
            return VK_ERROR_INITIALIZATION_FAILED;
    if (info->pEnabledFeatures) {
        // (No feature is supported: asking for one fails.)
        const VkBool32 *f = (const VkBool32 *)info->pEnabledFeatures;
        for (size_t k = 0; k < sizeof(VkPhysicalDeviceFeatures) / sizeof(VkBool32); k++)
            if (f[k])
                return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    struct VkDevice_T *d = zalloc(sizeof *d);
    if (!d)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    set_loader_magic_value(d);
    set_loader_magic_value(&d->queue);
    *out = d;
    return VK_SUCCESS;
}

static CALL void CC DestroyDevice(VkDevice d, const VkAllocationCallbacks *a) {
    free(d);
}

static CALL void CC GetDeviceQueue(VkDevice d, uint32_t family, uint32_t index, VkQueue *q) {
    *q = &d->queue;
}

// ---- Memory ----

static CALL VkResult CC AllocateMemory(VkDevice d, const VkMemoryAllocateInfo *info, const VkAllocationCallbacks *a, VkDeviceMemory *out) {
    if (info->memoryTypeIndex != 0)
        return VK_ERROR_OUT_OF_DEVICE_MEMORY;
    if (info->allocationSize > limits[L_ALLOCATION])
        return VK_ERROR_OUT_OF_DEVICE_MEMORY;
    struct VkDeviceMemory_T *m = zalloc(sizeof *m);
    if (!m)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    struct request q = {CREATE, {info->allocationSize}};
    if (call(&q)) {
        free(m);
        return VK_ERROR_OUT_OF_DEVICE_MEMORY;
    }
    m->handle = q.r[0];
    m->size = info->allocationSize;
    *out = m;
    return VK_SUCCESS;
}

static void unlist(VkDevice d, VkDeviceMemory m) {
    if (m->prev)
        m->prev->next = m->next;
    else if (d->mapped == m)
        d->mapped = m->next;
    if (m->next)
        m->next->prev = m->prev;
    m->next = m->prev = NULL;
}

static CALL void CC FreeMemory(VkDevice d, VkDeviceMemory m, const VkAllocationCallbacks *a) {
    if (!m)
        return;
    if (m->map) {
        unlist(d, m);
        free(m->map);
    }
    struct request q = {FREE, {m->handle}};
    call(&q);
    free(m->pending);
    free(m);
}

static int transfer(int op, VkDeviceMemory m, VkDeviceSize offset, VkDeviceSize end) {
    struct request q = {(uint64_t)op, {m->handle, offset, (uint64_t)(uintptr_t)(m->map + offset), end - offset}};
    return end > offset ? call(&q) : 0;
}

// The device's writes to a mapped memory read into the program's copy.
static int catch_up(VkDeviceMemory m) {
    int failed = 0;
    for (size_t k = 0; k < m->npending; k++)
        failed |= transfer(READ, m, m->pending[2 * k], m->pending[2 * k + 1]);
    m->npending = 0;
    return failed;
}

static CALL VkResult CC MapMemory(VkDevice d, VkDeviceMemory m, VkDeviceSize offset, VkDeviceSize size, VkMemoryMapFlags flags, void **out) {
    if (m->map)
        return VK_ERROR_MEMORY_MAP_FAILED;
    m->map = malloc(m->size ? m->size : 1);
    if (!m->map)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    if (transfer(READ, m, 0, m->size)) {
        free(m->map);
        m->map = NULL;
        return VK_ERROR_MEMORY_MAP_FAILED;
    }
    m->npending = 0;
    m->next = d->mapped;
    if (d->mapped)
        d->mapped->prev = m;
    d->mapped = m;
    *out = m->map + offset;
    return VK_SUCCESS;
}

static CALL void CC UnmapMemory(VkDevice d, VkDeviceMemory m) {
    if (!m->map)
        return;
    // (Unmapping makes the program's writes the device's.)
    catch_up(m);
    transfer(WRITE, m, 0, m->size);
    unlist(d, m);
    free(m->map);
    m->map = NULL;
}

static CALL VkResult CC FlushMappedMemoryRanges(VkDevice d, uint32_t n, const VkMappedMemoryRange *r) {
    for (uint32_t i = 0; i < n; i++) {
        VkDeviceMemory m = r[i].memory;
        VkDeviceSize end = r[i].size == VK_WHOLE_SIZE ? m->size : r[i].offset + r[i].size;
        if (m->map && transfer(WRITE, m, r[i].offset, end))
            return VK_ERROR_DEVICE_LOST;
    }
    return VK_SUCCESS;
}

static CALL VkResult CC InvalidateMappedMemoryRanges(VkDevice d, uint32_t n, const VkMappedMemoryRange *r) {
    for (uint32_t i = 0; i < n; i++)
        if (r[i].memory->map && catch_up(r[i].memory))
            return VK_ERROR_DEVICE_LOST;
    return VK_SUCCESS;
}

static CALL void CC GetDeviceMemoryCommitment(VkDevice d, VkDeviceMemory m, VkDeviceSize *bytes) {
    *bytes = m->size;
}

// ---- Buffers ----

static CALL VkResult CC CreateBuffer(VkDevice d, const VkBufferCreateInfo *info, const VkAllocationCallbacks *a, VkBuffer *out) {
    if (info->flags)
        return VK_ERROR_FEATURE_NOT_PRESENT;
    struct VkBuffer_T *b = zalloc(sizeof *b);
    if (!b)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    b->size = info->size;
    b->usage = info->usage;
    *out = b;
    return VK_SUCCESS;
}

static CALL void CC DestroyBuffer(VkDevice d, VkBuffer b, const VkAllocationCallbacks *a) {
    free(b);
}

static CALL void CC GetBufferMemoryRequirements(VkDevice d, VkBuffer b, VkMemoryRequirements *r) {
    VkDeviceSize align = limits[L_STORAGE_ALIGN] > limits[L_UNIFORM_ALIGN] ? limits[L_STORAGE_ALIGN] : limits[L_UNIFORM_ALIGN];
    r->alignment = align > 4 ? align : 4;
    r->size = (b->size + 3) & ~(VkDeviceSize)3;
    r->memoryTypeBits = 1;
}

static CALL VkResult CC BindBufferMemory(VkDevice d, VkBuffer b, VkDeviceMemory m, VkDeviceSize offset) {
    b->memory = m;
    b->offset = offset;
    return VK_SUCCESS;
}

// ---- Shaders and pipelines ----

static CALL VkResult CC CreateShaderModule(VkDevice d, const VkShaderModuleCreateInfo *info, const VkAllocationCallbacks *a, VkShaderModule *out) {
    struct VkShaderModule_T *s = zalloc(sizeof *s);
    if (!s)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    char why[256];
    struct request q = {VK_SHADER, {(uint64_t)(uintptr_t)info->pCode, info->codeSize, 0, (uint64_t)(uintptr_t)why, sizeof why}};
    if (call(&q)) {
        free(s);
        return VK_ERROR_INVALID_SHADER_NV;
    }
    s->handle = q.r[0];
    *out = s;
    return VK_SUCCESS;
}

static CALL void CC DestroyShaderModule(VkDevice d, VkShaderModule s, const VkAllocationCallbacks *a) {
    if (!s)
        return;
    struct request q = {FREE, {s->handle}};
    call(&q);
    free(s);
}

static CALL VkResult CC CreateComputePipelines(VkDevice d, VkPipelineCache cache, uint32_t n, const VkComputePipelineCreateInfo *info, const VkAllocationCallbacks *a, VkPipeline *out) {
    VkResult result = VK_SUCCESS;
    for (uint32_t i = 0; i < n; i++) {
        out[i] = VK_NULL_HANDLE;
        const VkPipelineShaderStageCreateInfo *st = &info[i].stage;
        char why[256];
        struct request q = {VK_PIPELINE, {st->module->handle, (uint64_t)(uintptr_t)st->pName, strlen(st->pName), (uint64_t)(uintptr_t)why, sizeof why}};
        struct VkPipeline_T *p = zalloc(sizeof *p);
        if (!p || call(&q)) {
            free(p);
            result = VK_ERROR_INITIALIZATION_FAILED;
            continue;
        }
        p->handle = q.r[0];
        out[i] = p;
    }
    return result;
}

static CALL VkResult CC CreateGraphicsPipelines(VkDevice d, VkPipelineCache cache, uint32_t n, const VkGraphicsPipelineCreateInfo *info, const VkAllocationCallbacks *a, VkPipeline *out) {
    for (uint32_t i = 0; i < n; i++)
        out[i] = VK_NULL_HANDLE;
    return VK_ERROR_FEATURE_NOT_PRESENT;
}

static CALL void CC DestroyPipeline(VkDevice d, VkPipeline p, const VkAllocationCallbacks *a) {
    if (!p)
        return;
    struct request q = {FREE, {p->handle}};
    call(&q);
    free(p);
}

static CALL VkResult CC CreatePipelineCache(VkDevice d, const VkPipelineCacheCreateInfo *info, const VkAllocationCallbacks *a, VkPipelineCache *out) {
    *out = zalloc(sizeof(struct VkPipelineCache_T));
    return *out ? VK_SUCCESS : VK_ERROR_OUT_OF_HOST_MEMORY;
}
static CALL void CC DestroyPipelineCache(VkDevice d, VkPipelineCache c, const VkAllocationCallbacks *a) {
    free(c);
}
static CALL VkResult CC GetPipelineCacheData(VkDevice d, VkPipelineCache c, size_t *size, void *data) {
    // (A header alone: nothing cached.)
    VkPipelineCacheHeaderVersionOne h = {sizeof h, VK_PIPELINE_CACHE_HEADER_VERSION_ONE, (uint32_t)limits[L_VENDOR], (uint32_t)limits[L_DEVICE], "wasm-env-vulkan"};
    if (!data) {
        *size = sizeof h;
        return VK_SUCCESS;
    }
    if (*size < sizeof h) {
        *size = 0;
        return VK_INCOMPLETE;
    }
    memcpy(data, &h, sizeof h);
    *size = sizeof h;
    return VK_SUCCESS;
}
static CALL VkResult CC MergePipelineCaches(VkDevice d, VkPipelineCache dst, uint32_t n, const VkPipelineCache *src) {
    return VK_SUCCESS;
}

static CALL VkResult CC CreatePipelineLayout(VkDevice d, const VkPipelineLayoutCreateInfo *info, const VkAllocationCallbacks *a, VkPipelineLayout *out) {
    if (info->setLayoutCount > 8)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    struct VkPipelineLayout_T *l = zalloc(sizeof *l);
    if (!l)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    l->sets = info->setLayoutCount;
    memcpy(l->layouts, info->pSetLayouts, info->setLayoutCount * sizeof *l->layouts);
    *out = l;
    return VK_SUCCESS;
}
static CALL void CC DestroyPipelineLayout(VkDevice d, VkPipelineLayout l, const VkAllocationCallbacks *a) {
    free(l);
}

// ---- Descriptor sets (of buffers) ----

static int buffer_descriptor(VkDescriptorType t) {
    return t == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER || t == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER || t == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC ||
           t == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC;
}

static CALL VkResult CC CreateDescriptorSetLayout(VkDevice d, const VkDescriptorSetLayoutCreateInfo *info, const VkAllocationCallbacks *a, VkDescriptorSetLayout *out) {
    for (uint32_t i = 0; i < info->bindingCount; i++)
        if (info->pBindings[i].descriptorCount > 1 || (info->pBindings[i].descriptorCount && !buffer_descriptor(info->pBindings[i].descriptorType)))
            return VK_ERROR_FEATURE_NOT_PRESENT;
    struct VkDescriptorSetLayout_T *l = zalloc(sizeof *l);
    if (!l || !(l->bindings = zalloc(info->bindingCount * sizeof *l->bindings))) {
        free(l);
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    // (In binding order: the order of dynamic offsets.)
    l->n = info->bindingCount;
    memcpy(l->bindings, info->pBindings, l->n * sizeof *l->bindings);
    for (uint32_t i = 1; i < l->n; i++)
        for (uint32_t j = i; j > 0 && l->bindings[j - 1].binding > l->bindings[j].binding; j--) {
            VkDescriptorSetLayoutBinding t = l->bindings[j];
            l->bindings[j] = l->bindings[j - 1];
            l->bindings[j - 1] = t;
        }
    *out = l;
    return VK_SUCCESS;
}
static CALL void CC DestroyDescriptorSetLayout(VkDevice d, VkDescriptorSetLayout l, const VkAllocationCallbacks *a) {
    if (l) {
        free(l->bindings);
        free(l);
    }
}

static CALL VkResult CC CreateDescriptorPool(VkDevice d, const VkDescriptorPoolCreateInfo *info, const VkAllocationCallbacks *a, VkDescriptorPool *out) {
    *out = zalloc(sizeof(struct VkDescriptorPool_T));
    return *out ? VK_SUCCESS : VK_ERROR_OUT_OF_HOST_MEMORY;
}

static void free_set(VkDescriptorSet s) {
    free(s->d);
    free(s);
}

static CALL VkResult CC ResetDescriptorPool(VkDevice d, VkDescriptorPool p, VkDescriptorPoolResetFlags f) {
    while (p->sets) {
        VkDescriptorSet s = p->sets;
        p->sets = s->next;
        free_set(s);
    }
    return VK_SUCCESS;
}
static CALL void CC DestroyDescriptorPool(VkDevice d, VkDescriptorPool p, const VkAllocationCallbacks *a) {
    if (p) {
        ResetDescriptorPool(d, p, 0);
        free(p);
    }
}

static CALL VkResult CC AllocateDescriptorSets(VkDevice d, const VkDescriptorSetAllocateInfo *info, VkDescriptorSet *out) {
    for (uint32_t i = 0; i < info->descriptorSetCount; i++) {
        struct VkDescriptorSet_T *s = zalloc(sizeof *s);
        VkDescriptorSetLayout l = info->pSetLayouts[i];
        if (!s || !(s->d = zalloc(l->n * sizeof *s->d))) {
            free(s);
            for (uint32_t j = 0; j < i; j++)
                out[j] = VK_NULL_HANDLE;
            return VK_ERROR_OUT_OF_POOL_MEMORY;
        }
        s->layout = l;
        s->pool = info->descriptorPool;
        s->next = s->pool->sets;
        s->pool->sets = s;
        for (uint32_t k = 0; k < l->n; k++)
            s->d[k].type = l->bindings[k].descriptorType;
        out[i] = s;
    }
    return VK_SUCCESS;
}

static CALL VkResult CC FreeDescriptorSets(VkDevice d, VkDescriptorPool p, uint32_t n, const VkDescriptorSet *sets) {
    for (uint32_t i = 0; i < n; i++) {
        if (!sets[i])
            continue;
        for (VkDescriptorSet *at = &p->sets; *at; at = &(*at)->next)
            if (*at == sets[i]) {
                *at = sets[i]->next;
                break;
            }
        free_set(sets[i]);
    }
    return VK_SUCCESS;
}

// A set's descriptor for a binding (and the binding after it, as writes
// of several descriptors continue to the next binding).
static struct descriptor *slot(VkDescriptorSet s, uint32_t binding) {
    for (uint32_t k = 0; k < s->layout->n; k++)
        if (s->layout->bindings[k].binding == binding)
            return &s->d[k];
    return NULL;
}

static CALL void CC UpdateDescriptorSets(VkDevice d, uint32_t nw, const VkWriteDescriptorSet *w, uint32_t nc, const VkCopyDescriptorSet *c) {
    for (uint32_t i = 0; i < nw; i++) {
        if (!buffer_descriptor(w[i].descriptorType))
            continue;
        for (uint32_t k = 0; k < w[i].descriptorCount; k++) {
            struct descriptor *e = slot(w[i].dstSet, w[i].dstBinding + k);
            if (!e)
                continue;
            const VkDescriptorBufferInfo *b = &w[i].pBufferInfo[k];
            e->buffer = b->buffer;
            e->offset = b->offset;
            e->range = b->range == VK_WHOLE_SIZE ? b->buffer->size - b->offset : b->range;
        }
    }
    for (uint32_t i = 0; i < nc; i++)
        for (uint32_t k = 0; k < c[i].descriptorCount; k++) {
            struct descriptor *from = slot(c[i].srcSet, c[i].srcBinding + k), *to = slot(c[i].dstSet, c[i].dstBinding + k);
            if (from && to)
                *to = *from;
        }
}

// ---- Command buffers ----

static CALL VkResult CC CreateCommandPool(VkDevice d, const VkCommandPoolCreateInfo *info, const VkAllocationCallbacks *a, VkCommandPool *out) {
    *out = zalloc(sizeof(struct VkCommandPool_T));
    return *out ? VK_SUCCESS : VK_ERROR_OUT_OF_HOST_MEMORY;
}
static CALL void CC DestroyCommandPool(VkDevice d, VkCommandPool p, const VkAllocationCallbacks *a) {
    free(p);
}
static CALL VkResult CC ResetCommandPool(VkDevice d, VkCommandPool p, VkCommandPoolResetFlags f) {
    return VK_SUCCESS;
}

static CALL VkResult CC AllocateCommandBuffers(VkDevice d, const VkCommandBufferAllocateInfo *info, VkCommandBuffer *out) {
    for (uint32_t i = 0; i < info->commandBufferCount; i++) {
        struct VkCommandBuffer_T *c = zalloc(sizeof *c);
        if (!c) {
            for (uint32_t j = 0; j < i; j++) {
                free(out[j]);
                out[j] = VK_NULL_HANDLE;
            }
            return VK_ERROR_OUT_OF_HOST_MEMORY;
        }
        set_loader_magic_value(c);
        out[i] = c;
    }
    return VK_SUCCESS;
}

static void reset(VkCommandBuffer c) {
    c->n = c->nwrites = 0;
    c->failed = 0;
}

static CALL void CC FreeCommandBuffers(VkDevice d, VkCommandPool p, uint32_t n, const VkCommandBuffer *cs) {
    for (uint32_t i = 0; i < n; i++)
        if (cs[i]) {
            free(cs[i]->s);
            free(cs[i]->writes);
            free(cs[i]);
        }
}

static CALL VkResult CC BeginCommandBuffer(VkCommandBuffer c, const VkCommandBufferBeginInfo *info) {
    reset(c);
    return VK_SUCCESS;
}
static CALL VkResult CC EndCommandBuffer(VkCommandBuffer c) {
    return c->failed ? VK_ERROR_OUT_OF_HOST_MEMORY : VK_SUCCESS;
}
static CALL VkResult CC ResetCommandBuffer(VkCommandBuffer c, VkCommandBufferResetFlags f) {
    reset(c);
    return VK_SUCCESS;
}

static void put(VkCommandBuffer c, const void *p, size_t n) {
    if (c->n + n > c->cap) {
        size_t cap = c->cap ? c->cap * 2 : 256;
        while (cap < c->n + n)
            cap *= 2;
        uint8_t *s = realloc(c->s, cap);
        if (!s) {
            c->failed = 1;
            return;
        }
        c->s = s;
        c->cap = cap;
    }
    memcpy(c->s + c->n, p, n);
    c->n += n;
}

// A command: its operation and length, then its operands (`body`, and
// `more` after them).
static void command(VkCommandBuffer c, uint32_t op, const void *body, uint32_t len, const void *more, uint32_t more_len) {
    uint32_t h[2] = {op, 8 + len + more_len};
    put(c, h, 8);
    put(c, body, len);
    if (more_len)
        put(c, more, more_len);
}

// That a command may write a buffer's range (read back after a wait, if
// its memory is mapped then).
static void writes(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, VkDeviceSize size) {
    if (c->nwrites == c->wcap) {
        size_t cap = c->wcap ? c->wcap * 2 : 16;
        struct write *w = realloc(c->writes, cap * sizeof *w);
        if (!w) {
            c->failed = 1;
            return;
        }
        c->writes = w;
        c->wcap = cap;
    }
    c->writes[c->nwrites++] = (struct write){b->memory, b->offset + offset, b->offset + offset + size};
}

static CALL void CC CmdBindPipeline(VkCommandBuffer c, VkPipelineBindPoint point, VkPipeline p) {
    if (point == VK_PIPELINE_BIND_POINT_COMPUTE)
        command(c, BIND_PIPELINE, &p->handle, 8, NULL, 0);
}

static CALL void CC CmdBindDescriptorSets(VkCommandBuffer c, VkPipelineBindPoint point, VkPipelineLayout layout, uint32_t first, uint32_t n, const VkDescriptorSet *sets, uint32_t ndyn, const uint32_t *dyn) {
    uint32_t used = 0;
    for (uint32_t i = 0; i < n; i++) {
        VkDescriptorSet s = sets[i];
        if (!s)
            continue;
        uint32_t count = 0;
        for (uint32_t k = 0; k < s->layout->n; k++)
            count += s->d[k].buffer != VK_NULL_HANDLE;
        uint32_t head[2] = {first + i, count};
        uint32_t h[2] = {BIND_SET, 8 + 8 + count * 32};
        put(c, h, 8);
        put(c, head, 8);
        for (uint32_t k = 0; k < s->layout->n; k++) {
            struct descriptor *e = &s->d[k];
            VkDeviceSize offset = e->offset;
            if (e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC || e->type == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC)
                offset += used < ndyn ? dyn[used++] : 0;
            if (!e->buffer)
                continue;
            uint32_t b[2] = {s->layout->bindings[k].binding, 0};
            uint64_t r[3] = {e->buffer->memory->handle, e->buffer->offset + offset, e->range};
            put(c, b, 8);
            put(c, r, 24);
            if (e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER || e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC)
                writes(c, e->buffer, offset, e->range);
        }
    }
}

static CALL void CC CmdPushConstants(VkCommandBuffer c, VkPipelineLayout l, VkShaderStageFlags stages, uint32_t offset, uint32_t size, const void *values) {
    uint32_t head[2] = {offset, size};
    command(c, PUSH, head, 8, values, size);
}

static CALL void CC CmdDispatch(VkCommandBuffer c, uint32_t x, uint32_t y, uint32_t z) {
    uint32_t g[3] = {x, y, z};
    command(c, DISPATCH, g, 12, NULL, 0);
}

static CALL void CC CmdDispatchIndirect(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset) {
    uint64_t o[2] = {b->memory->handle, b->offset + offset};
    command(c, DISPATCH_INDIRECT, o, 16, NULL, 0);
}

static CALL void CC CmdCopyBuffer(VkCommandBuffer c, VkBuffer src, VkBuffer dst, uint32_t n, const VkBufferCopy *r) {
    for (uint32_t i = 0; i < n; i++) {
        uint64_t o[5] = {src->memory->handle, src->offset + r[i].srcOffset, dst->memory->handle, dst->offset + r[i].dstOffset, r[i].size};
        command(c, COPY, o, 40, NULL, 0);
        writes(c, dst, r[i].dstOffset, r[i].size);
    }
}

static CALL void CC CmdFillBuffer(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, VkDeviceSize size, uint32_t word) {
    if (size == VK_WHOLE_SIZE)
        size = (b->size - offset) & ~(VkDeviceSize)3;
    uint64_t o[3] = {b->memory->handle, b->offset + offset, size};
    command(c, FILL, o, 24, &word, 4);
    writes(c, b, offset, size);
}

static CALL void CC CmdUpdateBuffer(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, VkDeviceSize size, const void *data) {
    struct {
        uint64_t o[2];
        uint32_t n;
    } __attribute__((packed)) head = {{b->memory->handle, b->offset + offset}, (uint32_t)size};
    command(c, UPDATE, &head, sizeof head, data, (uint32_t)size);
    writes(c, b, offset, size);
}

// Barriers and events: the commands run in order (wgpu orders a
// submission's work), so there's nothing to wait for.
static CALL void CC CmdPipelineBarrier(VkCommandBuffer c, VkPipelineStageFlags src, VkPipelineStageFlags dst, VkDependencyFlags f, uint32_t nm, const VkMemoryBarrier *m, uint32_t nb, const VkBufferMemoryBarrier *b, uint32_t ni, const VkImageMemoryBarrier *im) {}
static CALL void CC CmdSetEvent(VkCommandBuffer c, VkEvent e, VkPipelineStageFlags s) {}
static CALL void CC CmdResetEvent(VkCommandBuffer c, VkEvent e, VkPipelineStageFlags s) {}
static CALL void CC CmdWaitEvents(VkCommandBuffer c, uint32_t n, const VkEvent *e, VkPipelineStageFlags src, VkPipelineStageFlags dst, uint32_t nm, const VkMemoryBarrier *m, uint32_t nb, const VkBufferMemoryBarrier *b, uint32_t ni, const VkImageMemoryBarrier *im) {}

static CALL void CC CmdExecuteCommands(VkCommandBuffer c, uint32_t n, const VkCommandBuffer *cs) {
    for (uint32_t i = 0; i < n; i++) {
        put(c, cs[i]->s, cs[i]->n);
        for (size_t k = 0; k < cs[i]->nwrites; k++) {
            struct write w = cs[i]->writes[k];
            struct VkBuffer_T b = {w.end - w.offset, 0, w.memory, w.offset};
            writes(c, &b, 0, w.end - w.offset);
        }
    }
}

// ---- Submission and synchronization ----

// Before a submission: the device's writes to mapped memory read back,
// then the program's copies sent.
static int send_mapped(VkDevice d) {
    for (VkDeviceMemory m = d->mapped; m; m = m->next)
        if (catch_up(m) || transfer(WRITE, m, 0, m->size))
            return -1;
    return 0;
}

static void note_writes(VkCommandBuffer c) {
    for (size_t k = 0; k < c->nwrites; k++) {
        VkDeviceMemory m = c->writes[k].memory;
        if (m->npending == m->cap) {
            size_t cap = m->cap ? m->cap * 2 : 8;
            VkDeviceSize *p = realloc(m->pending, cap * 2 * sizeof *p);
            if (!p)
                continue;
            m->pending = p;
            m->cap = cap;
        }
        VkDeviceSize end = c->writes[k].end < m->size ? c->writes[k].end : m->size;
        m->pending[2 * m->npending] = c->writes[k].offset;
        m->pending[2 * m->npending + 1] = end;
        m->npending++;
    }
}

static VkDevice the_device(VkQueue q) {
    return (VkDevice)((char *)q - offsetof(struct VkDevice_T, queue));
}

static CALL VkResult CC QueueSubmit(VkQueue queue, uint32_t n, const VkSubmitInfo *info, VkFence fence) {
    VkDevice d = the_device(queue);
    if (send_mapped(d))
        return VK_ERROR_DEVICE_LOST;
    for (uint32_t i = 0; i < n; i++)
        for (uint32_t k = 0; k < info[i].commandBufferCount; k++) {
            VkCommandBuffer c = info[i].pCommandBuffers[k];
            if (!c->n)
                continue;
            struct request q = {VK_SUBMIT, {(uint64_t)(uintptr_t)c->s, c->n}};
            if (call(&q))
                return VK_ERROR_DEVICE_LOST;
            note_writes(c);
        }
    if (fence)
        fence->signaled = 1;
    return VK_SUCCESS;
}

// The device idle, and mapped memory up to date.
static VkResult idle(VkDevice d) {
    struct request q = {FINISH};
    if (call(&q))
        return VK_ERROR_DEVICE_LOST;
    for (VkDeviceMemory m = d->mapped; m; m = m->next)
        if (catch_up(m))
            return VK_ERROR_DEVICE_LOST;
    return VK_SUCCESS;
}

static CALL VkResult CC QueueWaitIdle(VkQueue q) {
    return idle(the_device(q));
}
static CALL VkResult CC DeviceWaitIdle(VkDevice d) {
    return idle(d);
}

static CALL VkResult CC CreateFence(VkDevice d, const VkFenceCreateInfo *info, const VkAllocationCallbacks *a, VkFence *out) {
    struct VkFence_T *f = zalloc(sizeof *f);
    if (!f)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    f->signaled = (info->flags & VK_FENCE_CREATE_SIGNALED_BIT) != 0;
    *out = f;
    return VK_SUCCESS;
}
static CALL void CC DestroyFence(VkDevice d, VkFence f, const VkAllocationCallbacks *a) {
    free(f);
}
static CALL VkResult CC ResetFences(VkDevice d, uint32_t n, const VkFence *fs) {
    for (uint32_t i = 0; i < n; i++)
        fs[i]->signaled = 0;
    return VK_SUCCESS;
}
// (A fence's submission is done once the device is idle.)
static CALL VkResult CC GetFenceStatus(VkDevice d, VkFence f) {
    if (!f->signaled)
        return VK_NOT_READY;
    return idle(d);
}
static CALL VkResult CC WaitForFences(VkDevice d, uint32_t n, const VkFence *fs, VkBool32 all, uint64_t timeout) {
    int any = 0, every = 1;
    for (uint32_t i = 0; i < n; i++) {
        any |= fs[i]->signaled;
        every &= fs[i]->signaled;
    }
    // (Unsubmitted fences never signal: nothing else can signal them.)
    if (all ? !every : !any)
        return VK_TIMEOUT;
    return idle(d);
}

static CALL VkResult CC CreateSemaphore(VkDevice d, const VkSemaphoreCreateInfo *info, const VkAllocationCallbacks *a, VkSemaphore *out) {
    *out = zalloc(sizeof(struct VkSemaphore_T));
    return *out ? VK_SUCCESS : VK_ERROR_OUT_OF_HOST_MEMORY;
}
static CALL void CC DestroySemaphore(VkDevice d, VkSemaphore s, const VkAllocationCallbacks *a) {
    free(s);
}

static CALL VkResult CC CreateEvent(VkDevice d, const VkEventCreateInfo *info, const VkAllocationCallbacks *a, VkEvent *out) {
    *out = zalloc(sizeof(struct VkEvent_T));
    return *out ? VK_SUCCESS : VK_ERROR_OUT_OF_HOST_MEMORY;
}
static CALL void CC DestroyEvent(VkDevice d, VkEvent e, const VkAllocationCallbacks *a) {
    free(e);
}
static CALL VkResult CC GetEventStatus(VkDevice d, VkEvent e) {
    return e->set ? VK_EVENT_SET : VK_EVENT_RESET;
}
static CALL VkResult CC SetEvent(VkDevice d, VkEvent e) {
    e->set = 1;
    return VK_SUCCESS;
}
static CALL VkResult CC ResetEvent(VkDevice d, VkEvent e) {
    e->set = 0;
    return VK_SUCCESS;
}

// ---- Entry points ----

static CALL PFN_vkVoidFunction CC GetDeviceProcAddr(VkDevice d, const char *name);

#define F(name) {"vk" #name, (PFN_vkVoidFunction)name}
#define FKHR(name) {"vk" #name "KHR", (PFN_vkVoidFunction)name}
static const struct entry {
    const char *name;
    PFN_vkVoidFunction f;
} global[] = {
    F(CreateInstance),
    F(EnumerateInstanceExtensionProperties),
    F(EnumerateInstanceVersion),
},
  instance[] = {
      F(DestroyInstance),
      F(EnumeratePhysicalDevices),
      F(GetPhysicalDeviceProperties),
      F(GetPhysicalDeviceFeatures),
      F(GetPhysicalDeviceQueueFamilyProperties),
      F(GetPhysicalDeviceMemoryProperties),
      F(GetPhysicalDeviceFormatProperties),
      F(GetPhysicalDeviceImageFormatProperties),
      F(GetPhysicalDeviceSparseImageFormatProperties),
      FKHR(GetPhysicalDeviceProperties2),
      FKHR(GetPhysicalDeviceFeatures2),
      FKHR(GetPhysicalDeviceMemoryProperties2),
      FKHR(GetPhysicalDeviceFormatProperties2),
      FKHR(GetPhysicalDeviceQueueFamilyProperties2),
      FKHR(GetPhysicalDeviceImageFormatProperties2),
      FKHR(GetPhysicalDeviceSparseImageFormatProperties2),
      F(EnumerateDeviceExtensionProperties),
      F(EnumerateDeviceLayerProperties),
      F(CreateDevice),
      F(GetDeviceProcAddr),
},
  device[] = {
      F(DestroyDevice),
      F(GetDeviceQueue),
      F(QueueSubmit),
      F(QueueWaitIdle),
      F(DeviceWaitIdle),
      F(AllocateMemory),
      F(FreeMemory),
      F(MapMemory),
      F(UnmapMemory),
      F(FlushMappedMemoryRanges),
      F(InvalidateMappedMemoryRanges),
      F(GetDeviceMemoryCommitment),
      F(CreateBuffer),
      F(DestroyBuffer),
      F(GetBufferMemoryRequirements),
      F(BindBufferMemory),
      F(CreateShaderModule),
      F(DestroyShaderModule),
      F(CreateComputePipelines),
      F(CreateGraphicsPipelines),
      F(DestroyPipeline),
      F(CreatePipelineCache),
      F(DestroyPipelineCache),
      F(GetPipelineCacheData),
      F(MergePipelineCaches),
      F(CreatePipelineLayout),
      F(DestroyPipelineLayout),
      F(CreateDescriptorSetLayout),
      F(DestroyDescriptorSetLayout),
      F(CreateDescriptorPool),
      F(DestroyDescriptorPool),
      F(ResetDescriptorPool),
      F(AllocateDescriptorSets),
      F(FreeDescriptorSets),
      F(UpdateDescriptorSets),
      F(CreateCommandPool),
      F(DestroyCommandPool),
      F(ResetCommandPool),
      F(AllocateCommandBuffers),
      F(FreeCommandBuffers),
      F(BeginCommandBuffer),
      F(EndCommandBuffer),
      F(ResetCommandBuffer),
      F(CmdBindPipeline),
      F(CmdBindDescriptorSets),
      F(CmdPushConstants),
      F(CmdDispatch),
      F(CmdDispatchIndirect),
      F(CmdCopyBuffer),
      F(CmdFillBuffer),
      F(CmdUpdateBuffer),
      F(CmdPipelineBarrier),
      F(CmdSetEvent),
      F(CmdResetEvent),
      F(CmdWaitEvents),
      F(CmdExecuteCommands),
      F(CreateFence),
      F(DestroyFence),
      F(ResetFences),
      F(GetFenceStatus),
      F(WaitForFences),
      F(CreateSemaphore),
      F(DestroySemaphore),
      F(CreateEvent),
      F(DestroyEvent),
      F(GetEventStatus),
      F(SetEvent),
      F(ResetEvent),
};

static PFN_vkVoidFunction find(const struct entry *t, size_t n, const char *name) {
    for (size_t k = 0; k < n; k++)
        if (!strcmp(t[k].name, name))
            return t[k].f;
    return NULL;
}
#define FIND(t, name) find(t, sizeof t / sizeof *t, name)

static CALL PFN_vkVoidFunction CC GetDeviceProcAddr(VkDevice d, const char *name) {
    return FIND(device, name);
}

API CALL PFN_vkVoidFunction CC vk_icdGetInstanceProcAddr(VkInstance i, const char *name) {
    PFN_vkVoidFunction f = FIND(global, name);
    if (f || !i)
        return f;
    if ((f = FIND(instance, name)))
        return f;
    return FIND(device, name);
}

API CALL PFN_vkVoidFunction CC vk_icdGetPhysicalDeviceProcAddr(VkInstance i, const char *name) {
    return FIND(instance, name);
}

// The loader's interface: version 5 (it asks for the physical device's
// functions through vk_icdGetPhysicalDeviceProcAddr).
API CALL VkResult CC vk_icdNegotiateLoaderICDInterfaceVersion(uint32_t *version) {
    if (*version > 5)
        *version = 5;
    return *version < 4 ? VK_ERROR_INCOMPATIBLE_DRIVER : VK_SUCCESS;
}
