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
int close(int);
#define O_RDWR 02
#define O_CLOEXEC 02000000

#define API __attribute__((visibility("default")))
#define GPU_CALL 0xc0805740u
#define CALL VKAPI_ATTR
#define CC VKAPI_CALL

enum { INFO = 1, CREATE, FREE, WRITE, READ, FINISH = 13, VK_SHADER = 20, VK_PIPELINE, VK_SUBMIT, VK_LIMITS, VK_IMAGE, VK_VIEW, VK_SAMPLER, VK_FORMAT, VK_GRAPHICS };
// The command stream's operations (gpu/src/vulkan.rs, `decode`).
enum {
    BIND_PIPELINE = 1,
    BIND_SET,
    PUSH,
    DISPATCH,
    DISPATCH_INDIRECT,
    COPY,
    FILL,
    UPDATE,
    BUFFER_TO_IMAGE,
    IMAGE_TO_BUFFER,
    COPY_IMAGE,
    CLEAR_COLOR,
    BEGIN_PASS,
    END_PASS,
    SET_VIEWPORT,
    SET_SCISSOR,
    BIND_VERTEX,
    BIND_INDEX,
    DRAW,
    DRAW_INDEXED,
    DRAW_INDIRECT,
    DRAW_INDEXED_INDIRECT,
    SET_BLEND_CONSTANTS,
    SET_STENCIL_REFERENCE,
    BIND_GRAPHICS_SET
};

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
    // (A Vulkan request first: the runtime opens the GPU for the API that
    // asks first. It fails where there's no GPU.)
    if (call(&l) || call(&q)) {
        close(f);
        fd = -1;
        return -1;
    }
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
    struct VkDevice_T *device;
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
// Graphics state a pipeline may leave to commands (Vulkan's dynamic
// state), kept by command buffers: WebGPU's passes each start without it.
enum { S_VIEWPORT = 1, S_SCISSOR = 2, S_BLEND = 4, S_REFERENCE = 8 };
struct state {
    uint32_t set;
    float viewport[6];
    uint32_t scissor[4];
    float blend[4];
    uint32_t reference;
};
struct VkPipeline_T {
    uint64_t handle;
    int graphics;
    // A graphics pipeline's own state (its `set` what isn't dynamic).
    struct state fixed;
};
struct subpass {
    uint32_t ncolors;
    uint32_t colors[8];
    uint32_t depth; // VK_ATTACHMENT_UNUSED: none
};
struct VkRenderPass_T {
    uint32_t n;
    VkAttachmentDescription *attachments;
    // Per attachment: the subpasses that first and last use it.
    uint32_t *first, *last;
    uint32_t nsub;
    struct subpass *sub;
};
struct VkFramebuffer_T {
    uint32_t n;
    VkImageView *views;
    uint32_t width, height;
};
struct VkPipelineLayout_T {
    uint32_t sets;
    VkDescriptorSetLayout layouts[8];
};
struct VkDescriptorSetLayout_T {
    uint32_t n;
    VkDescriptorSetLayoutBinding *bindings;
    // Per binding: its immutable sampler (or NULL).
    VkSampler *immutable;
};
struct VkImage_T {
    uint64_t handle;
    VkImageType type;
    VkFormat format;
    VkExtent3D extent;
    uint32_t mips, layers, texel;
};
struct VkImageView_T {
    uint64_t handle;
};
struct VkSampler_T {
    uint64_t handle;
};
// A descriptor: a buffer's range, an image view, a sampler, or a view and
// sampler.
struct descriptor {
    VkDescriptorType type;
    VkBuffer buffer;
    VkDeviceSize offset, range;
    VkImageView view;
    VkSampler sampler;
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
    int failed;
    // Graphics: the state set, and the render pass begun (or NULL), its
    // framebuffer, subpass and clear values.
    struct state state;
    VkRenderPass pass;
    VkFramebuffer framebuffer;
    uint32_t subpass;
    VkClearValue *clears;
    uint32_t nclears;
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

// (VK_KHR_wayland_surface's name, without vulkan_wayland.h.)
static const VkExtensionProperties instance_extensions[] = {
    {VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME, VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_SPEC_VERSION},
    {VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_SURFACE_SPEC_VERSION},
    {"VK_KHR_wayland_surface", 6},
};
static const VkExtensionProperties device_extensions[] = {
    {VK_KHR_SWAPCHAIN_EXTENSION_NAME, VK_KHR_SWAPCHAIN_SPEC_VERSION},
};

// Whether every extension named is one of `have`.
static int extensions(const VkExtensionProperties *have, size_t n, uint32_t count, const char *const *names) {
    for (uint32_t i = 0; i < count; i++) {
        size_t k = 0;
        while (k < n && strcmp(names[i], have[k].extensionName))
            k++;
        if (k == n)
            return 0;
    }
    return 1;
}

static CALL VkResult CC CreateInstance(const VkInstanceCreateInfo *info, const VkAllocationCallbacks *a, VkInstance *out) {
    if (!extensions(instance_extensions, sizeof instance_extensions / sizeof *instance_extensions, info->enabledExtensionCount, info->ppEnabledExtensionNames))
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

static CALL VkResult CC EnumerateInstanceExtensionProperties(const char *layer, uint32_t *count, VkExtensionProperties *out) {
    if (layer)
        return VK_ERROR_LAYER_NOT_PRESENT;
    return enumerate(count, out, instance_extensions, sizeof instance_extensions / sizeof *instance_extensions, sizeof *out);
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

static const VkQueueFamilyProperties family = {VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT, 1, 0, {1, 1, 1}};

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

// A format's features for optimal tiling (the device's: VK_FORMAT), and
// its texel's bytes (0: not supported).
static VkFormatFeatureFlags format_features(VkFormat f, uint32_t *texel) {
    struct request q = {VK_FORMAT, {(uint64_t)f}};
    if (call(&q))
        q.r[0] = q.r[2] = 0;
    if (texel)
        *texel = (uint32_t)q.r[2];
    return (VkFormatFeatureFlags)q.r[0];
}

// (Images are optimal tiling only: linear ones would be memory the
// program reads and writes as an image.)
static CALL void CC GetPhysicalDeviceFormatProperties(VkPhysicalDevice pd, VkFormat f, VkFormatProperties *p) {
    memset(p, 0, sizeof *p);
    p->optimalTilingFeatures = format_features(f, NULL);
}

// The features an image's usage needs.
static VkFormatFeatureFlags needs(VkImageUsageFlags u) {
    VkFormatFeatureFlags n = 0;
    if (u & VK_IMAGE_USAGE_TRANSFER_SRC_BIT)
        n |= VK_FORMAT_FEATURE_TRANSFER_SRC_BIT;
    if (u & VK_IMAGE_USAGE_TRANSFER_DST_BIT)
        n |= VK_FORMAT_FEATURE_TRANSFER_DST_BIT;
    if (u & VK_IMAGE_USAGE_SAMPLED_BIT)
        n |= VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT;
    if (u & VK_IMAGE_USAGE_STORAGE_BIT)
        n |= VK_FORMAT_FEATURE_STORAGE_IMAGE_BIT;
    if (u & VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT)
        n |= VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT;
    if (u & VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT)
        n |= VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT;
    return n;
}

static CALL VkResult CC GetPhysicalDeviceImageFormatProperties(VkPhysicalDevice pd, VkFormat f, VkImageType t, VkImageTiling tl, VkImageUsageFlags u, VkImageCreateFlags c, VkImageFormatProperties *p) {
    VkFormatFeatureFlags have = format_features(f, NULL);
    if (tl != VK_IMAGE_TILING_OPTIMAL || !have || (have & needs(u)) != needs(u) || (c & ~VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT))
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    memset(p, 0, sizeof *p);
    p->maxExtent = t == VK_IMAGE_TYPE_3D ? (VkExtent3D){256, 256, 256} : t == VK_IMAGE_TYPE_1D ? (VkExtent3D){4096, 1, 1} : (VkExtent3D){4096, 4096, 1};
    p->maxMipLevels = t == VK_IMAGE_TYPE_3D ? 9 : 13;
    p->maxArrayLayers = t == VK_IMAGE_TYPE_3D ? 1 : 256;
    p->sampleCounts = VK_SAMPLE_COUNT_1_BIT;
    p->maxResourceSize = (VkDeviceSize)1 << 31;
    return VK_SUCCESS;
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
    return GetPhysicalDeviceImageFormatProperties(pd, i->format, i->type, i->tiling, i->usage, i->flags, &p->imageFormatProperties);
}
static CALL void CC GetPhysicalDeviceSparseImageFormatProperties2(VkPhysicalDevice pd, const VkPhysicalDeviceSparseImageFormatInfo2 *i, uint32_t *count, VkSparseImageFormatProperties2 *p) {
    *count = 0;
}

static CALL VkResult CC EnumerateDeviceExtensionProperties(VkPhysicalDevice pd, const char *layer, uint32_t *count, VkExtensionProperties *out) {
    if (layer)
        return VK_ERROR_LAYER_NOT_PRESENT;
    return enumerate(count, out, device_extensions, sizeof device_extensions / sizeof *device_extensions, sizeof *out);
}

static CALL VkResult CC EnumerateDeviceLayerProperties(VkPhysicalDevice pd, uint32_t *count, VkLayerProperties *out) {
    *count = 0;
    return VK_SUCCESS;
}

// ---- Device and queue ----

static CALL VkResult CC CreateDevice(VkPhysicalDevice pd, const VkDeviceCreateInfo *info, const VkAllocationCallbacks *a, VkDevice *out) {
    if (!extensions(device_extensions, sizeof device_extensions / sizeof *device_extensions, info->enabledExtensionCount, info->ppEnabledExtensionNames))
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
    d->queue.device = d;
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

// ---- Images, views and samplers ----

// An image is a texture in the runtime, not part of its memory: binding
// memory to it changes nothing.
static CALL VkResult CC CreateImage(VkDevice d, const VkImageCreateInfo *info, const VkAllocationCallbacks *a, VkImage *out) {
    uint32_t texel;
    if (info->tiling != VK_IMAGE_TILING_OPTIMAL || !format_features(info->format, &texel))
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    struct VkImage_T *im = zalloc(sizeof *im);
    if (!im)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    uint32_t w[9] = {info->imageType == VK_IMAGE_TYPE_1D ? 1u : info->imageType == VK_IMAGE_TYPE_3D ? 3u : 2u,
                     (uint32_t)info->format,
                     info->extent.width,
                     info->extent.height,
                     info->extent.depth,
                     info->mipLevels,
                     info->arrayLayers,
                     (uint32_t)info->samples,
                     info->usage};
    struct request q = {VK_IMAGE, {(uint64_t)(uintptr_t)w}};
    if (call(&q)) {
        free(im);
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    *im = (struct VkImage_T){q.r[0], info->imageType, info->format, info->extent, info->mipLevels, info->arrayLayers, texel};
    *out = im;
    return VK_SUCCESS;
}

static CALL void CC DestroyImage(VkDevice d, VkImage im, const VkAllocationCallbacks *a) {
    if (!im)
        return;
    struct request q = {FREE, {im->handle}};
    call(&q);
    free(im);
}

static CALL void CC GetImageMemoryRequirements(VkDevice d, VkImage im, VkMemoryRequirements *r) {
    // (What its texels would take: the program allocates that much, which
    // the image doesn't use.)
    VkDeviceSize size = 0;
    for (uint32_t m = 0; m < im->mips; m++) {
        VkDeviceSize w = im->extent.width >> m, h = im->extent.height >> m, z = im->extent.depth >> m;
        size += (w ? w : 1) * (h ? h : 1) * (z ? z : 1) * im->layers * im->texel;
    }
    r->size = (size + 255) & ~(VkDeviceSize)255;
    r->alignment = 256;
    r->memoryTypeBits = 1;
}

static CALL VkResult CC BindImageMemory(VkDevice d, VkImage im, VkDeviceMemory m, VkDeviceSize offset) {
    return VK_SUCCESS;
}

static CALL void CC GetImageSubresourceLayout(VkDevice d, VkImage im, const VkImageSubresource *s, VkSubresourceLayout *l) {
    memset(l, 0, sizeof *l);
}

static CALL void CC GetImageSparseMemoryRequirements(VkDevice d, VkImage im, uint32_t *count, VkSparseImageMemoryRequirements *r) {
    *count = 0;
}

static CALL VkResult CC CreateImageView(VkDevice d, const VkImageViewCreateInfo *info, const VkAllocationCallbacks *a, VkImageView *out) {
    VkImage im = info->image;
    const VkImageSubresourceRange *r = &info->subresourceRange;
    uint32_t mips = r->levelCount == VK_REMAINING_MIP_LEVELS ? im->mips - r->baseMipLevel : r->levelCount;
    uint32_t layers = r->layerCount == VK_REMAINING_ARRAY_LAYERS ? im->layers - r->baseArrayLayer : r->layerCount;
    uint32_t w[7] = {(uint32_t)info->viewType, info->format == im->format ? 0u : (uint32_t)info->format, r->aspectMask, r->baseMipLevel, mips, r->baseArrayLayer, layers};
    struct request q = {VK_VIEW, {im->handle, (uint64_t)(uintptr_t)w}};
    struct VkImageView_T *v = zalloc(sizeof *v);
    if (!v || call(&q)) {
        free(v);
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    v->handle = q.r[0];
    *out = v;
    return VK_SUCCESS;
}

static CALL void CC DestroyImageView(VkDevice d, VkImageView v, const VkAllocationCallbacks *a) {
    if (!v)
        return;
    struct request q = {FREE, {v->handle}};
    call(&q);
    free(v);
}

static CALL VkResult CC CreateSampler(VkDevice d, const VkSamplerCreateInfo *info, const VkAllocationCallbacks *a, VkSampler *out) {
    union {
        float f;
        uint32_t u;
    } lo = {info->minLod}, hi = {info->maxLod};
    uint32_t w[10] = {(uint32_t)info->magFilter,
                      (uint32_t)info->minFilter,
                      (uint32_t)info->mipmapMode,
                      (uint32_t)info->addressModeU,
                      (uint32_t)info->addressModeV,
                      (uint32_t)info->addressModeW,
                      info->anisotropyEnable ? (uint32_t)info->maxAnisotropy : 1u,
                      info->compareEnable ? (uint32_t)info->compareOp + 1 : 0u,
                      lo.u,
                      hi.u};
    struct request q = {VK_SAMPLER, {(uint64_t)(uintptr_t)w}};
    struct VkSampler_T *s = zalloc(sizeof *s);
    if (!s || call(&q)) {
        free(s);
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    s->handle = q.r[0];
    *out = s;
    return VK_SUCCESS;
}

static CALL void CC DestroySampler(VkDevice d, VkSampler s, const VkAllocationCallbacks *a) {
    if (!s)
        return;
    struct request q = {FREE, {s->handle}};
    call(&q);
    free(s);
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

// Words for VK_GRAPHICS (gpu/src/vulkan.rs, `GraphicsState::decode`).
struct words {
    uint32_t *w;
    size_t n, cap;
    int failed;
};

static void word(struct words *b, uint32_t v) {
    if (b->n == b->cap) {
        size_t cap = b->cap ? b->cap * 2 : 128;
        uint32_t *w = realloc(b->w, cap * sizeof *w);
        if (!w) {
            b->failed = 1;
            return;
        }
        b->w = w;
        b->cap = cap;
    }
    b->w[b->n++] = v;
}

static uint32_t bits(float f) {
    union {
        float f;
        uint32_t u;
    } x = {f};
    return x.u;
}

static void stencil_face(struct words *b, const VkStencilOpState *s) {
    word(b, (uint32_t)s->failOp);
    word(b, (uint32_t)s->passOp);
    word(b, (uint32_t)s->depthFailOp);
    word(b, (uint32_t)s->compareOp);
    word(b, s->compareMask);
    word(b, s->writeMask);
}

static void name(struct words *b, const char *s) {
    size_t n = strlen(s);
    for (size_t k = 0; k < n; k += 4) {
        uint32_t w = 0;
        memcpy(&w, s + k, n - k < 4 ? n - k : 4);
        word(b, w);
    }
}

static VkResult graphics_pipeline(const VkGraphicsPipelineCreateInfo *info, VkPipeline *out) {
    const VkPipelineShaderStageCreateInfo *vs = NULL, *fs = NULL;
    for (uint32_t k = 0; k < info->stageCount; k++) {
        const VkPipelineShaderStageCreateInfo *st = &info->pStages[k];
        if (st->stage == VK_SHADER_STAGE_VERTEX_BIT)
            vs = st;
        else if (st->stage == VK_SHADER_STAGE_FRAGMENT_BIT)
            fs = st;
        else
            return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    const VkPipelineRasterizationStateCreateInfo *rs = info->pRasterizationState;
    if (!vs || !rs)
        return VK_ERROR_INITIALIZATION_FAILED;
    if (rs->rasterizerDiscardEnable)
        fs = NULL;
    const struct subpass *sp = &info->renderPass->sub[info->subpass];
    const VkAttachmentDescription *att = info->renderPass->attachments;
    int has_depth = sp->depth != VK_ATTACHMENT_UNUSED;
    const VkPipelineDepthStencilStateCreateInfo *ds = has_depth ? info->pDepthStencilState : NULL;
    const VkPipelineColorBlendStateCreateInfo *cb = fs ? info->pColorBlendState : NULL;
    const VkPipelineVertexInputStateCreateInfo *vi = info->pVertexInputState;
    struct words b = {0};
    uint64_t shaders[2] = {vs->module->handle, fs ? fs->module->handle : 0};
    for (int k = 0; k < 2; k++) {
        word(&b, (uint32_t)shaders[k]);
        word(&b, (uint32_t)(shaders[k] >> 32));
    }
    word(&b, strlen(vs->pName));
    word(&b, fs ? strlen(fs->pName) : 0);
    word(&b, (uint32_t)info->pInputAssemblyState->topology);
    word(&b, rs->cullMode);
    word(&b, (uint32_t)rs->frontFace);
    word(&b, rs->depthBiasEnable);
    word(&b, bits(rs->depthBiasConstantFactor));
    word(&b, bits(rs->depthBiasSlopeFactor));
    word(&b, bits(rs->depthBiasClamp));
    word(&b, ds ? ds->depthTestEnable : 0);
    word(&b, ds ? ds->depthWriteEnable : 0);
    word(&b, ds ? (uint32_t)ds->depthCompareOp : 0);
    word(&b, ds ? ds->stencilTestEnable : 0);
    VkStencilOpState none = {0};
    stencil_face(&b, ds ? &ds->front : &none);
    stencil_face(&b, ds ? &ds->back : &none);
    word(&b, has_depth ? (uint32_t)att[sp->depth].format : 0);
    word(&b, info->pMultisampleState ? (uint32_t)info->pMultisampleState->rasterizationSamples : 1);
    uint32_t nc = fs ? sp->ncolors : 0, nb = vi ? vi->vertexBindingDescriptionCount : 0, na = vi ? vi->vertexAttributeDescriptionCount : 0;
    word(&b, nc);
    word(&b, nb);
    word(&b, na);
    for (uint32_t k = 0; k < nc; k++) {
        word(&b, (uint32_t)att[sp->colors[k]].format);
        VkPipelineColorBlendAttachmentState blend = {.colorWriteMask = 0xf};
        if (cb && k < cb->attachmentCount)
            blend = cb->pAttachments[k];
        word(&b, blend.blendEnable);
        word(&b, (uint32_t)blend.srcColorBlendFactor);
        word(&b, (uint32_t)blend.dstColorBlendFactor);
        word(&b, (uint32_t)blend.colorBlendOp);
        word(&b, (uint32_t)blend.srcAlphaBlendFactor);
        word(&b, (uint32_t)blend.dstAlphaBlendFactor);
        word(&b, (uint32_t)blend.alphaBlendOp);
        word(&b, blend.colorWriteMask);
    }
    for (uint32_t k = 0; k < nb; k++) {
        const VkVertexInputBindingDescription *v = &vi->pVertexBindingDescriptions[k];
        word(&b, v->binding);
        word(&b, v->stride);
        word(&b, (uint32_t)v->inputRate);
    }
    for (uint32_t k = 0; k < na; k++) {
        const VkVertexInputAttributeDescription *v = &vi->pVertexAttributeDescriptions[k];
        word(&b, v->location);
        word(&b, v->binding);
        word(&b, (uint32_t)v->format);
        word(&b, v->offset);
    }
    name(&b, vs->pName);
    if (fs)
        name(&b, fs->pName);
    struct VkPipeline_T *p = zalloc(sizeof *p);
    char why[256];
    struct request q = {VK_GRAPHICS, {(uint64_t)(uintptr_t)b.w, b.n * 4, 0, (uint64_t)(uintptr_t)why, sizeof why}};
    if (!p || b.failed || call(&q)) {
        free(b.w);
        free(p);
        return b.failed || !p ? VK_ERROR_OUT_OF_HOST_MEMORY : VK_ERROR_INITIALIZATION_FAILED;
    }
    free(b.w);
    p->handle = q.r[0];
    p->graphics = 1;
    // Its own state: what it doesn't leave to commands.
    struct state *s = &p->fixed;
    s->set = S_VIEWPORT | S_SCISSOR | S_BLEND | S_REFERENCE;
    if (info->pDynamicState)
        for (uint32_t k = 0; k < info->pDynamicState->dynamicStateCount; k++)
            switch (info->pDynamicState->pDynamicStates[k]) {
            case VK_DYNAMIC_STATE_VIEWPORT: s->set &= ~S_VIEWPORT; break;
            case VK_DYNAMIC_STATE_SCISSOR: s->set &= ~S_SCISSOR; break;
            case VK_DYNAMIC_STATE_BLEND_CONSTANTS: s->set &= ~S_BLEND; break;
            case VK_DYNAMIC_STATE_STENCIL_REFERENCE: s->set &= ~S_REFERENCE; break;
            default: break;
            }
    const VkPipelineViewportStateCreateInfo *vp = info->pViewportState;
    if (vp && vp->pViewports && vp->viewportCount)
        memcpy(s->viewport, vp->pViewports, sizeof s->viewport);
    else
        s->set &= ~S_VIEWPORT;
    if (vp && vp->pScissors && vp->scissorCount)
        memcpy(s->scissor, vp->pScissors, sizeof s->scissor);
    else
        s->set &= ~S_SCISSOR;
    if (cb)
        memcpy(s->blend, cb->blendConstants, sizeof s->blend);
    else
        s->set &= ~S_BLEND;
    if (ds && ds->stencilTestEnable)
        s->reference = ds->front.reference;
    else
        s->set &= ~S_REFERENCE;
    *out = p;
    return VK_SUCCESS;
}

static CALL VkResult CC CreateGraphicsPipelines(VkDevice d, VkPipelineCache cache, uint32_t n, const VkGraphicsPipelineCreateInfo *info, const VkAllocationCallbacks *a, VkPipeline *out) {
    VkResult result = VK_SUCCESS;
    for (uint32_t i = 0; i < n; i++) {
        out[i] = VK_NULL_HANDLE;
        VkResult r = graphics_pipeline(&info[i], &out[i]);
        if (r != VK_SUCCESS)
            result = r;
    }
    return result;
}

// ---- Render passes and framebuffers ----

static CALL VkResult CC CreateRenderPass(VkDevice d, const VkRenderPassCreateInfo *info, const VkAllocationCallbacks *a, VkRenderPass *out) {
    struct VkRenderPass_T *p = zalloc(sizeof *p);
    if (!p)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    p->n = info->attachmentCount;
    p->nsub = info->subpassCount;
    p->attachments = malloc((p->n ? p->n : 1) * sizeof *p->attachments);
    p->first = malloc((p->n ? p->n : 1) * sizeof *p->first);
    p->last = malloc((p->n ? p->n : 1) * sizeof *p->last);
    p->sub = zalloc(p->nsub * sizeof *p->sub);
    if (!p->attachments || !p->first || !p->last || !p->sub) {
        free(p->attachments), free(p->first), free(p->last), free(p->sub), free(p);
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    if (p->n)
        memcpy(p->attachments, info->pAttachments, p->n * sizeof *p->attachments);
    for (uint32_t k = 0; k < p->n; k++)
        p->first[k] = VK_ATTACHMENT_UNUSED, p->last[k] = 0;
    for (uint32_t s = 0; s < p->nsub; s++) {
        const VkSubpassDescription *sd = &info->pSubpasses[s];
        struct subpass *sub = &p->sub[s];
        // (Multisampled attachments resolved: not yet.)
        if (sd->pResolveAttachments)
            for (uint32_t k = 0; k < sd->colorAttachmentCount; k++)
                if (sd->pResolveAttachments[k].attachment != VK_ATTACHMENT_UNUSED) {
                    free(p->attachments), free(p->first), free(p->last), free(p->sub), free(p);
                    return VK_ERROR_FEATURE_NOT_PRESENT;
                }
        for (uint32_t k = 0; k < sd->colorAttachmentCount && sub->ncolors < 8; k++)
            if (sd->pColorAttachments[k].attachment != VK_ATTACHMENT_UNUSED)
                sub->colors[sub->ncolors++] = sd->pColorAttachments[k].attachment;
        sub->depth = sd->pDepthStencilAttachment ? sd->pDepthStencilAttachment->attachment : VK_ATTACHMENT_UNUSED;
        for (uint32_t k = 0; k <= sub->ncolors; k++) {
            uint32_t at = k < sub->ncolors ? sub->colors[k] : sub->depth;
            if (at == VK_ATTACHMENT_UNUSED)
                continue;
            if (p->first[at] == VK_ATTACHMENT_UNUSED)
                p->first[at] = s;
            p->last[at] = s;
        }
    }
    *out = p;
    return VK_SUCCESS;
}

static CALL void CC DestroyRenderPass(VkDevice d, VkRenderPass p, const VkAllocationCallbacks *a) {
    if (!p)
        return;
    free(p->attachments), free(p->first), free(p->last), free(p->sub), free(p);
}

static CALL void CC GetRenderAreaGranularity(VkDevice d, VkRenderPass p, VkExtent2D *g) {
    *g = (VkExtent2D){1, 1};
}

static CALL VkResult CC CreateFramebuffer(VkDevice d, const VkFramebufferCreateInfo *info, const VkAllocationCallbacks *a, VkFramebuffer *out) {
    struct VkFramebuffer_T *f = zalloc(sizeof *f);
    VkImageView *views = malloc((info->attachmentCount ? info->attachmentCount : 1) * sizeof *views);
    if (!f || !views) {
        free(f), free(views);
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    if (info->attachmentCount)
        memcpy(views, info->pAttachments, info->attachmentCount * sizeof *views);
    f->n = info->attachmentCount;
    f->views = views;
    f->width = info->width;
    f->height = info->height;
    *out = f;
    return VK_SUCCESS;
}

static CALL void CC DestroyFramebuffer(VkDevice d, VkFramebuffer f, const VkAllocationCallbacks *a) {
    if (!f)
        return;
    free(f->views), free(f);
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

static int image_descriptor(VkDescriptorType t) {
    return t == VK_DESCRIPTOR_TYPE_SAMPLER || t == VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER || t == VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE ||
           t == VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
}

static CALL VkResult CC CreateDescriptorSetLayout(VkDevice d, const VkDescriptorSetLayoutCreateInfo *info, const VkAllocationCallbacks *a, VkDescriptorSetLayout *out) {
    for (uint32_t i = 0; i < info->bindingCount; i++) {
        const VkDescriptorSetLayoutBinding *b = &info->pBindings[i];
        if (b->descriptorCount > 1 || (b->descriptorCount && !buffer_descriptor(b->descriptorType) && !image_descriptor(b->descriptorType)))
            return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    struct VkDescriptorSetLayout_T *l = zalloc(sizeof *l);
    if (!l || !(l->bindings = zalloc(info->bindingCount * sizeof *l->bindings)) || !(l->immutable = zalloc(info->bindingCount * sizeof *l->immutable))) {
        if (l)
            free(l->bindings);
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
    // (Immutable samplers kept: the program's array needn't outlive this.)
    for (uint32_t k = 0; k < l->n; k++) {
        VkDescriptorSetLayoutBinding *b = &l->bindings[k];
        int has = (b->descriptorType == VK_DESCRIPTOR_TYPE_SAMPLER || b->descriptorType == VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER) && b->pImmutableSamplers && b->descriptorCount;
        l->immutable[k] = has ? b->pImmutableSamplers[0] : VK_NULL_HANDLE;
        b->pImmutableSamplers = NULL;
    }
    *out = l;
    return VK_SUCCESS;
}
static CALL void CC DestroyDescriptorSetLayout(VkDevice d, VkDescriptorSetLayout l, const VkAllocationCallbacks *a) {
    if (l) {
        free(l->bindings);
        free(l->immutable);
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
        for (uint32_t k = 0; k < l->n; k++) {
            s->d[k].type = l->bindings[k].descriptorType;
            s->d[k].sampler = l->immutable[k];
        }
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
        VkDescriptorType t = w[i].descriptorType;
        for (uint32_t k = 0; k < w[i].descriptorCount; k++) {
            struct descriptor *e = slot(w[i].dstSet, w[i].dstBinding + k);
            if (!e)
                continue;
            if (buffer_descriptor(t)) {
                const VkDescriptorBufferInfo *b = &w[i].pBufferInfo[k];
                e->buffer = b->buffer;
                e->offset = b->offset;
                e->range = b->range == VK_WHOLE_SIZE ? b->buffer->size - b->offset : b->range;
            } else if (image_descriptor(t)) {
                const VkDescriptorImageInfo *im = &w[i].pImageInfo[k];
                if (t != VK_DESCRIPTOR_TYPE_SAMPLER)
                    e->view = im->imageView;
                // (An immutable sampler stays.)
                if ((t == VK_DESCRIPTOR_TYPE_SAMPLER || t == VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER) && !w[i].dstSet->layout->immutable[e - w[i].dstSet->d])
                    e->sampler = im->sampler;
            }
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
    c->state.set = 0;
    c->pass = NULL;
}

static CALL void CC FreeCommandBuffers(VkDevice d, VkCommandPool p, uint32_t n, const VkCommandBuffer *cs) {
    for (uint32_t i = 0; i < n; i++)
        if (cs[i]) {
            free(cs[i]->s);
            free(cs[i]->writes);
            free(cs[i]->clears);
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

// Graphics state set (the bits `which`), into the render pass begun.
static void emit_state(VkCommandBuffer c, uint32_t which) {
    struct state *s = &c->state;
    which &= s->set;
    if (!c->pass)
        return;
    if (which & S_VIEWPORT)
        command(c, SET_VIEWPORT, s->viewport, sizeof s->viewport, NULL, 0);
    if (which & S_SCISSOR) {
        // (Within the framebuffer, as WebGPU's must be.)
        int32_t x = (int32_t)s->scissor[0], y = (int32_t)s->scissor[1];
        int64_t w = s->scissor[2], h = s->scissor[3];
        if (x < 0)
            w += x, x = 0;
        if (y < 0)
            h += y, y = 0;
        uint32_t fw = c->framebuffer->width, fh = c->framebuffer->height;
        if ((uint32_t)x > fw)
            x = (int32_t)fw;
        if ((uint32_t)y > fh)
            y = (int32_t)fh;
        if (w > (int64_t)fw - x)
            w = (int64_t)fw - x;
        if (h > (int64_t)fh - y)
            h = (int64_t)fh - y;
        uint32_t r[4] = {(uint32_t)x, (uint32_t)y, (uint32_t)(w > 0 ? w : 0), (uint32_t)(h > 0 ? h : 0)};
        command(c, SET_SCISSOR, r, sizeof r, NULL, 0);
    }
    if (which & S_BLEND)
        command(c, SET_BLEND_CONSTANTS, s->blend, sizeof s->blend, NULL, 0);
    if (which & S_REFERENCE)
        command(c, SET_STENCIL_REFERENCE, &s->reference, 4, NULL, 0);
}

static CALL void CC CmdBindPipeline(VkCommandBuffer c, VkPipelineBindPoint point, VkPipeline p) {
    command(c, BIND_PIPELINE, &p->handle, 8, NULL, 0);
    if (!p->graphics)
        return;
    // Its own state, as if set by commands.
    struct state *s = &c->state, *f = &p->fixed;
    if (f->set & S_VIEWPORT)
        memcpy(s->viewport, f->viewport, sizeof s->viewport);
    if (f->set & S_SCISSOR)
        memcpy(s->scissor, f->scissor, sizeof s->scissor);
    if (f->set & S_BLEND)
        memcpy(s->blend, f->blend, sizeof s->blend);
    if (f->set & S_REFERENCE)
        s->reference = f->reference;
    s->set |= f->set;
    emit_state(c, f->set);
}

static CALL void CC CmdBindDescriptorSets(VkCommandBuffer c, VkPipelineBindPoint point, VkPipelineLayout layout, uint32_t first, uint32_t n, const VkDescriptorSet *sets, uint32_t ndyn, const uint32_t *dyn) {
    uint32_t used = 0;
    for (uint32_t i = 0; i < n; i++) {
        VkDescriptorSet s = sets[i];
        if (!s)
            continue;
        // Its descriptors that are set (kind 0 a buffer, 1 a view, 2 a
        // sampler, 3 a view and sampler: gpu/src/vulkan.rs, `decode`).
        uint32_t count = 0;
        for (uint32_t k = 0; k < s->layout->n; k++)
            count += s->d[k].buffer || s->d[k].view || s->d[k].sampler;
        uint32_t head[2] = {first + i, count};
        uint32_t h[2] = {point == VK_PIPELINE_BIND_POINT_GRAPHICS ? BIND_GRAPHICS_SET : BIND_SET, 8 + 8 + count * 32};
        put(c, h, 8);
        put(c, head, 8);
        for (uint32_t k = 0; k < s->layout->n; k++) {
            struct descriptor *e = &s->d[k];
            VkDeviceSize offset = e->offset;
            if (e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC || e->type == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC)
                offset += used < ndyn ? dyn[used++] : 0;
            uint32_t b[2] = {s->layout->bindings[k].binding, 0};
            uint64_t r[3] = {0, 0, 0};
            if (e->buffer) {
                r[0] = e->buffer->memory->handle;
                r[1] = e->buffer->offset + offset;
                r[2] = e->range;
                if (e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER || e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC)
                    writes(c, e->buffer, offset, e->range);
            } else if (e->view && e->sampler) {
                b[1] = 3;
                r[0] = e->view->handle;
                r[1] = e->sampler->handle;
            } else if (e->view) {
                b[1] = 1;
                r[0] = e->view->handle;
            } else if (e->sampler) {
                b[1] = 2;
                r[0] = e->sampler->handle;
            } else
                continue;
            put(c, b, 8);
            put(c, r, 24);
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

// An image's region in a command (a SUB: gpu/src/vulkan.rs, `decode`).
static void sub(VkCommandBuffer c, const VkImageSubresourceLayers *s, VkOffset3D o, VkExtent3D e) {
    uint32_t w[10] = {s->mipLevel, s->baseArrayLayer, s->layerCount, s->aspectMask, (uint32_t)o.x, (uint32_t)o.y, (uint32_t)o.z, e.width, e.height, e.depth};
    put(c, w, sizeof w);
}

static void buffer_image(VkCommandBuffer c, uint32_t op, VkBuffer b, VkImage im, uint32_t n, const VkBufferImageCopy *r) {
    for (uint32_t i = 0; i < n; i++) {
        uint32_t h[2] = {op, 8 + 24 + 8 + 40};
        uint64_t o[3] = {b->memory->handle, b->offset + r[i].bufferOffset, im->handle};
        uint32_t rows[2] = {r[i].bufferRowLength, r[i].bufferImageHeight};
        put(c, h, 8);
        put(c, o, 24);
        put(c, rows, 8);
        sub(c, &r[i].imageSubresource, r[i].imageOffset, r[i].imageExtent);
        if (op == IMAGE_TO_BUFFER) {
            // (The bytes the copy reaches in the buffer.)
            VkExtent3D e = r[i].imageExtent;
            VkDeviceSize row = (VkDeviceSize)(r[i].bufferRowLength ? r[i].bufferRowLength : e.width) * im->texel;
            VkDeviceSize height = r[i].bufferImageHeight ? r[i].bufferImageHeight : e.height;
            VkDeviceSize slices = im->type == VK_IMAGE_TYPE_3D ? e.depth : r[i].imageSubresource.layerCount;
            writes(c, b, r[i].bufferOffset, (slices - 1) * height * row + (e.height - 1) * row + (VkDeviceSize)e.width * im->texel);
        }
    }
}

static CALL void CC CmdCopyBufferToImage(VkCommandBuffer c, VkBuffer b, VkImage im, VkImageLayout l, uint32_t n, const VkBufferImageCopy *r) {
    buffer_image(c, BUFFER_TO_IMAGE, b, im, n, r);
}

static CALL void CC CmdCopyImageToBuffer(VkCommandBuffer c, VkImage im, VkImageLayout l, VkBuffer b, uint32_t n, const VkBufferImageCopy *r) {
    buffer_image(c, IMAGE_TO_BUFFER, b, im, n, r);
}

static CALL void CC CmdCopyImage(VkCommandBuffer c, VkImage src, VkImageLayout sl, VkImage dst, VkImageLayout dl, uint32_t n, const VkImageCopy *r) {
    for (uint32_t i = 0; i < n; i++) {
        uint32_t h[2] = {COPY_IMAGE, 8 + 8 + 40 + 8 + 40};
        put(c, h, 8);
        put(c, &src->handle, 8);
        sub(c, &r[i].srcSubresource, r[i].srcOffset, r[i].extent);
        put(c, &dst->handle, 8);
        sub(c, &r[i].dstSubresource, r[i].dstOffset, r[i].extent);
    }
}

static CALL void CC CmdClearColorImage(VkCommandBuffer c, VkImage im, VkImageLayout l, const VkClearColorValue *color, uint32_t n, const VkImageSubresourceRange *r) {
    for (uint32_t i = 0; i < n; i++) {
        uint32_t mips = r[i].levelCount == VK_REMAINING_MIP_LEVELS ? im->mips - r[i].baseMipLevel : r[i].levelCount;
        uint32_t layers = r[i].layerCount == VK_REMAINING_ARRAY_LAYERS ? im->layers - r[i].baseArrayLayer : r[i].layerCount;
        uint32_t w[8] = {r[i].baseMipLevel, mips, r[i].baseArrayLayer, layers, color->uint32[0], color->uint32[1], color->uint32[2], color->uint32[3]};
        command(c, CLEAR_COLOR, &im->handle, 8, w, sizeof w);
    }
}

// ---- Drawing ----

// A subpass begun: a WebGPU render pass of its attachments, which load as
// the render pass says where it first uses them (else what's there), and
// store unless where it last does the render pass says not to.
static void begin_subpass(VkCommandBuffer c) {
    VkRenderPass p = c->pass;
    VkFramebuffer f = c->framebuffer;
    uint32_t s = c->subpass;
    const struct subpass *sp = &p->sub[s];
    uint32_t head[2] = {sp->ncolors, sp->depth != VK_ATTACHMENT_UNUSED};
    uint32_t h[2] = {BEGIN_PASS, 8 + 8 + sp->ncolors * 32 + (head[1] ? 32 : 0)};
    put(c, h, 8);
    put(c, head, 8);
    VkClearValue zero = {0};
    for (uint32_t k = 0; k <= sp->ncolors; k++) {
        uint32_t at = k < sp->ncolors ? sp->colors[k] : sp->depth;
        if (at == VK_ATTACHMENT_UNUSED)
            continue;
        const VkAttachmentDescription *d = &p->attachments[at];
        const VkClearValue *clear = at < c->nclears ? &c->clears[at] : &zero;
        uint32_t load = p->first[at] == s ? (uint32_t)d->loadOp : 0, store = p->last[at] == s ? (uint32_t)d->storeOp : 0;
        uint32_t w[6];
        if (k < sp->ncolors) {
            w[0] = load, w[1] = store;
            memcpy(w + 2, clear->color.uint32, 16);
        } else {
            uint32_t sload = p->first[at] == s ? (uint32_t)d->stencilLoadOp : 0, sstore = p->last[at] == s ? (uint32_t)d->stencilStoreOp : 0;
            w[0] = load, w[1] = store, w[2] = sload, w[3] = sstore;
            w[4] = bits(clear->depthStencil.depth), w[5] = clear->depthStencil.stencil;
        }
        put(c, &f->views[at]->handle, 8);
        put(c, w, sizeof w);
    }
    // (State set before: each WebGPU pass starts without it.)
    emit_state(c, ~0u);
}

static CALL void CC CmdBeginRenderPass(VkCommandBuffer c, const VkRenderPassBeginInfo *info, VkSubpassContents contents) {
    if (info->clearValueCount > c->nclears) {
        VkClearValue *v = realloc(c->clears, info->clearValueCount * sizeof *v);
        if (!v) {
            c->failed = 1;
            return;
        }
        c->clears = v;
    }
    c->nclears = info->clearValueCount;
    if (c->nclears)
        memcpy(c->clears, info->pClearValues, c->nclears * sizeof *c->clears);
    c->pass = info->renderPass;
    c->framebuffer = info->framebuffer;
    c->subpass = 0;
    begin_subpass(c);
}

static CALL void CC CmdNextSubpass(VkCommandBuffer c, VkSubpassContents contents) {
    command(c, END_PASS, NULL, 0, NULL, 0);
    c->subpass++;
    begin_subpass(c);
}

static CALL void CC CmdEndRenderPass(VkCommandBuffer c) {
    command(c, END_PASS, NULL, 0, NULL, 0);
    c->pass = NULL;
}

static void set_state(VkCommandBuffer c, uint32_t bit) {
    c->state.set |= bit;
    emit_state(c, bit);
}

// (One viewport and scissor: the device's limit.)
static CALL void CC CmdSetViewport(VkCommandBuffer c, uint32_t first, uint32_t n, const VkViewport *v) {
    if (first == 0 && n) {
        memcpy(c->state.viewport, v, sizeof c->state.viewport);
        set_state(c, S_VIEWPORT);
    }
}

static CALL void CC CmdSetScissor(VkCommandBuffer c, uint32_t first, uint32_t n, const VkRect2D *r) {
    if (first == 0 && n) {
        memcpy(c->state.scissor, r, sizeof c->state.scissor);
        set_state(c, S_SCISSOR);
    }
}

static CALL void CC CmdSetBlendConstants(VkCommandBuffer c, const float blend[4]) {
    memcpy(c->state.blend, blend, sizeof c->state.blend);
    set_state(c, S_BLEND);
}

static CALL void CC CmdSetStencilReference(VkCommandBuffer c, VkStencilFaceFlags faces, uint32_t reference) {
    c->state.reference = reference;
    set_state(c, S_REFERENCE);
}

// State WebGPU's pipelines fix (their own, then): lines one pixel wide,
// no depth bounds test.
static CALL void CC CmdSetLineWidth(VkCommandBuffer c, float width) {}
static CALL void CC CmdSetDepthBias(VkCommandBuffer c, float constant, float clamp, float slope) {}
static CALL void CC CmdSetDepthBounds(VkCommandBuffer c, float lo, float hi) {}
static CALL void CC CmdSetStencilCompareMask(VkCommandBuffer c, VkStencilFaceFlags faces, uint32_t mask) {}
static CALL void CC CmdSetStencilWriteMask(VkCommandBuffer c, VkStencilFaceFlags faces, uint32_t mask) {}

static CALL void CC CmdBindVertexBuffers(VkCommandBuffer c, uint32_t first, uint32_t n, const VkBuffer *b, const VkDeviceSize *offsets) {
    uint32_t head[2] = {first, n};
    uint32_t h[2] = {BIND_VERTEX, 8 + 8 + n * 16};
    put(c, h, 8);
    put(c, head, 8);
    for (uint32_t k = 0; k < n; k++) {
        uint64_t r[2] = {b[k]->memory->handle, b[k]->offset + offsets[k]};
        put(c, r, 16);
    }
}

static CALL void CC CmdBindIndexBuffer(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, VkIndexType type) {
    uint64_t r[2] = {b->memory->handle, b->offset + offset};
    uint32_t t = (uint32_t)type;
    command(c, BIND_INDEX, r, 16, &t, 4);
}

static CALL void CC CmdDraw(VkCommandBuffer c, uint32_t vertices, uint32_t instances, uint32_t first, uint32_t first_instance) {
    uint32_t w[4] = {vertices, instances, first, first_instance};
    command(c, DRAW, w, sizeof w, NULL, 0);
}

static CALL void CC CmdDrawIndexed(VkCommandBuffer c, uint32_t indices, uint32_t instances, uint32_t first, int32_t vertex_offset, uint32_t first_instance) {
    uint32_t w[5] = {indices, instances, first, (uint32_t)vertex_offset, first_instance};
    command(c, DRAW_INDEXED, w, sizeof w, NULL, 0);
}

static void draw_indirect(VkCommandBuffer c, uint32_t op, VkBuffer b, VkDeviceSize offset, uint32_t count, uint32_t stride) {
    uint64_t r[2] = {b->memory->handle, b->offset + offset};
    uint32_t w[2] = {count, stride};
    command(c, op, r, 16, w, 8);
}

static CALL void CC CmdDrawIndirect(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, uint32_t count, uint32_t stride) {
    draw_indirect(c, DRAW_INDIRECT, b, offset, count, stride);
}

static CALL void CC CmdDrawIndexedIndirect(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, uint32_t count, uint32_t stride) {
    draw_indirect(c, DRAW_INDEXED_INDIRECT, b, offset, count, stride);
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
    return q->device;
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

// ---- Presenting: Wayland surfaces and swapchains ----
//
// As Mesa presents for its software drivers: a swapchain's images are
// ordinary images, and presenting one copies it into a shared-memory
// buffer (wl_shm) committed to the program's wl_surface, through the
// program's own libwayland-client on a queue of the driver's.

void *dlopen(const char *, int);
void *dlsym(void *, const char *);
int memfd_create(const char *, unsigned);
int ftruncate(int, long);
void *mmap(void *, size_t, int, int, int, long);
int munmap(void *, size_t);
#define RTLD_NOW 2
#define MFD_CLOEXEC 1u
#define PROT_READ 1
#define PROT_WRITE 2
#define MAP_SHARED 1
#define MAP_FAILED ((void *)-1)

// (The loader makes the surface: VkIcdSurfaceWayland, without
// wayland-client.h.)
struct wayland_surface {
    VkIcdSurfaceBase base;
    void *display, *surface;
};

struct wl_interface {
    const char *name;
    int version;
};
enum { WL_MARSHAL_FLAG_DESTROY = 1, WL_SHM_FORMAT_ARGB8888 = 0, WL_SHM_FORMAT_XRGB8888 = 1 };
// libwayland-client's functions and interfaces, found when first needed.
static struct {
    void *(*create_queue)(void *display);
    void (*queue_destroy)(void *queue);
    void *(*create_wrapper)(void *proxy);
    void (*wrapper_destroy)(void *wrapper);
    void (*set_queue)(void *proxy, void *queue);
    void *(*marshal_flags)(void *proxy, uint32_t opcode, const struct wl_interface *iface, uint32_t version, uint32_t flags, ...);
    int (*add_listener)(void *proxy, const void *implementation, void *data);
    void (*destroy)(void *proxy);
    uint32_t (*get_version)(void *proxy);
    int (*roundtrip_queue)(void *display, void *queue);
    int (*dispatch_queue)(void *display, void *queue);
    int (*flush)(void *display);
    const struct wl_interface *registry, *shm, *shm_pool, *buffer, *callback;
} wl;

static int load_wayland(void) {
    if (wl.callback)
        return 0;
    void *lib = dlopen("libwayland-client.so.0", RTLD_NOW);
    if (!lib)
        return -1;
    static const char *names[] = {"wl_display_create_queue",    "wl_event_queue_destroy", "wl_proxy_create_wrapper",  "wl_proxy_wrapper_destroy",
                                  "wl_proxy_set_queue",         "wl_proxy_marshal_flags", "wl_proxy_add_listener",    "wl_proxy_destroy",
                                  "wl_proxy_get_version",       "wl_display_roundtrip_queue", "wl_display_dispatch_queue", "wl_display_flush",
                                  "wl_registry_interface",      "wl_shm_interface",       "wl_shm_pool_interface",    "wl_buffer_interface",
                                  "wl_callback_interface"};
    void *found[sizeof names / sizeof *names];
    for (size_t k = 0; k < sizeof names / sizeof *names; k++)
        if (!(found[k] = dlsym(lib, names[k])))
            return -1;
    _Static_assert(sizeof wl == sizeof found, "one pointer each");
    memcpy(&wl, found, sizeof wl);
    return 0;
}

static CALL VkResult CC GetPhysicalDeviceSurfaceSupportKHR(VkPhysicalDevice pd, uint32_t family, VkSurfaceKHR surface, VkBool32 *supported) {
    *supported = ((VkIcdSurfaceBase *)surface)->platform == VK_ICD_WSI_PLATFORM_WAYLAND;
    return VK_SUCCESS;
}

static CALL VkBool32 CC GetPhysicalDeviceWaylandPresentationSupportKHR(VkPhysicalDevice pd, uint32_t family, void *display) {
    return VK_TRUE;
}

static const VkImageUsageFlags swapchain_usage =
    VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT;

// (Wayland's surfaces take the swapchain's size: no current extent.)
static CALL VkResult CC GetPhysicalDeviceSurfaceCapabilitiesKHR(VkPhysicalDevice pd, VkSurfaceKHR surface, VkSurfaceCapabilitiesKHR *c) {
    *c = (VkSurfaceCapabilitiesKHR){.minImageCount = 2,
                                    .maxImageCount = 8,
                                    .currentExtent = {0xffffffffu, 0xffffffffu},
                                    .minImageExtent = {1, 1},
                                    .maxImageExtent = {4096, 4096},
                                    .maxImageArrayLayers = 1,
                                    .supportedTransforms = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
                                    .currentTransform = VK_SURFACE_TRANSFORM_IDENTITY_BIT_KHR,
                                    .supportedCompositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR | VK_COMPOSITE_ALPHA_PRE_MULTIPLIED_BIT_KHR,
                                    .supportedUsageFlags = swapchain_usage};
    return VK_SUCCESS;
}

// wl_shm's formats, which are BGRA in memory.
static const VkSurfaceFormatKHR surface_formats[] = {
    {VK_FORMAT_B8G8R8A8_SRGB, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR},
    {VK_FORMAT_B8G8R8A8_UNORM, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR},
};

static CALL VkResult CC GetPhysicalDeviceSurfaceFormatsKHR(VkPhysicalDevice pd, VkSurfaceKHR surface, uint32_t *count, VkSurfaceFormatKHR *out) {
    return enumerate(count, out, surface_formats, 2, sizeof *out);
}

static CALL VkResult CC GetPhysicalDeviceSurfacePresentModesKHR(VkPhysicalDevice pd, VkSurfaceKHR surface, uint32_t *count, VkPresentModeKHR *out) {
    static const VkPresentModeKHR fifo = VK_PRESENT_MODE_FIFO_KHR;
    return enumerate(count, out, &fifo, 1, sizeof fifo);
}

struct slot {
    VkImage image;
    void *buffer;
    // The compositor has its buffer; the program has acquired it.
    int busy, acquired;
};
struct VkSwapchainKHR_T {
    VkDevice device;
    void *display, *queue, *surface, *shm, *pool, *frame;
    uint32_t width, height, n;
    struct slot *slots;
    uint8_t *pixels;
    size_t size;
    // The device's copy of an image presented, read into its buffer.
    struct VkDeviceMemory_T copy;
    uint32_t next;
    int lost;
};

static void registry_global(void *data, void *registry, uint32_t name, const char *interface, uint32_t version) {
    struct VkSwapchainKHR_T *s = data;
    if (!strcmp(interface, "wl_shm") && !s->shm)
        s->shm = wl.marshal_flags(registry, 0, wl.shm, 1, 0, name, wl.shm->name, 1u, NULL);
}
static void registry_global_remove(void *data, void *registry, uint32_t name) {}
static const struct {
    void (*global)(void *, void *, uint32_t, const char *, uint32_t);
    void (*global_remove)(void *, void *, uint32_t);
} registry_listener = {registry_global, registry_global_remove};

static void released(void *data, void *buffer) {
    ((struct slot *)data)->busy = 0;
}
static const struct {
    void (*release)(void *, void *);
} buffer_listener = {released};

static void frame_done(void *data, void *callback, uint32_t time) {
    struct VkSwapchainKHR_T *s = data;
    wl.destroy(callback);
    s->frame = NULL;
}
static const struct {
    void (*done)(void *, void *, uint32_t);
} frame_listener = {frame_done};

static CALL void CC DestroySwapchainKHR(VkDevice d, VkSwapchainKHR s, const VkAllocationCallbacks *a) {
    if (!s)
        return;
    for (uint32_t k = 0; k < s->n; k++) {
        if (s->slots[k].buffer)
            wl.marshal_flags(s->slots[k].buffer, 0, NULL, 1, WL_MARSHAL_FLAG_DESTROY);
        DestroyImage(d, s->slots[k].image, NULL);
    }
    if (s->pool)
        wl.marshal_flags(s->pool, 1, NULL, 1, WL_MARSHAL_FLAG_DESTROY);
    if (s->frame)
        wl.destroy(s->frame);
    if (s->shm)
        wl.destroy(s->shm);
    if (s->surface)
        wl.wrapper_destroy(s->surface);
    if (s->display)
        wl.flush(s->display);
    if (s->queue)
        wl.queue_destroy(s->queue);
    if (s->pixels)
        munmap(s->pixels, s->size);
    if (s->copy.handle) {
        struct request q = {FREE, {s->copy.handle}};
        call(&q);
    }
    free(s->slots);
    free(s);
}

static CALL VkResult CC CreateSwapchainKHR(VkDevice d, const VkSwapchainCreateInfoKHR *info, const VkAllocationCallbacks *a, VkSwapchainKHR *out) {
    struct wayland_surface *surface = (struct wayland_surface *)info->surface;
    if (surface->base.platform != VK_ICD_WSI_PLATFORM_WAYLAND || load_wayland())
        return VK_ERROR_SURFACE_LOST_KHR;
    struct VkSwapchainKHR_T *s = zalloc(sizeof *s);
    uint32_t n = info->minImageCount < 2 ? 2 : info->minImageCount;
    if (!s || !(s->slots = zalloc(n * sizeof *s->slots))) {
        free(s);
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    s->device = d;
    s->n = n;
    s->width = info->imageExtent.width;
    s->height = info->imageExtent.height;
    VkResult r = VK_ERROR_SURFACE_LOST_KHR;
    // wl_shm, found on a queue of the driver's, and the program's surface
    // on it too (for frame callbacks).
    s->queue = wl.create_queue(surface->display);
    void *display = wl.create_wrapper(surface->display);
    if (!s->queue || !display)
        goto fail;
    s->display = surface->display;
    wl.set_queue(display, s->queue);
    void *registry = wl.marshal_flags(display, 1, wl.registry, wl.get_version(display), 0, NULL);
    wl.wrapper_destroy(display);
    if (!registry)
        goto fail;
    wl.add_listener(registry, &registry_listener, s);
    wl.roundtrip_queue(s->display, s->queue);
    wl.destroy(registry);
    s->surface = wl.create_wrapper(surface->surface);
    if (!s->shm || !s->surface)
        goto fail;
    wl.set_queue(s->surface, s->queue);
    // The buffers: one shared memory, an image's pixels each.
    size_t stride = (size_t)s->width * 4, image = stride * s->height;
    s->size = image * n;
    int fd = memfd_create("wasm-env-vulkan", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, (long)s->size) || (s->pixels = mmap(NULL, s->size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)) == MAP_FAILED) {
        s->pixels = NULL;
        if (fd >= 0)
            close(fd);
        r = VK_ERROR_OUT_OF_HOST_MEMORY;
        goto fail;
    }
    s->pool = wl.marshal_flags(s->shm, 0, wl.shm_pool, 1, 0, NULL, fd, (int32_t)s->size);
    close(fd);
    uint32_t format = info->compositeAlpha == VK_COMPOSITE_ALPHA_PRE_MULTIPLIED_BIT_KHR ? WL_SHM_FORMAT_ARGB8888 : WL_SHM_FORMAT_XRGB8888;
    struct request q = {CREATE, {image}};
    if (!s->pool || call(&q))
        goto fail;
    s->copy.handle = q.r[0];
    s->copy.size = image;
    for (uint32_t k = 0; k < n; k++) {
        struct slot *sl = &s->slots[k];
        sl->buffer = wl.marshal_flags(s->pool, 0, wl.buffer, 1, 0, NULL, (int32_t)(image * k), (int32_t)s->width, (int32_t)s->height, (int32_t)stride, format);
        if (!sl->buffer)
            goto fail;
        wl.add_listener(sl->buffer, &buffer_listener, sl);
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                                .imageType = VK_IMAGE_TYPE_2D,
                                .format = info->imageFormat,
                                .extent = {s->width, s->height, 1},
                                .mipLevels = 1,
                                .arrayLayers = 1,
                                .samples = VK_SAMPLE_COUNT_1_BIT,
                                .tiling = VK_IMAGE_TILING_OPTIMAL,
                                .usage = info->imageUsage | VK_IMAGE_USAGE_TRANSFER_SRC_BIT};
        if ((r = CreateImage(d, &ii, NULL, &sl->image)) != VK_SUCCESS)
            goto fail;
    }
    wl.flush(s->display);
    *out = s;
    return VK_SUCCESS;
fail:
    DestroySwapchainKHR(d, s, NULL);
    return r;
}

static CALL VkResult CC GetSwapchainImagesKHR(VkDevice d, VkSwapchainKHR s, uint32_t *count, VkImage *out) {
    if (!out) {
        *count = s->n;
        return VK_SUCCESS;
    }
    uint32_t n = *count < s->n ? *count : s->n;
    for (uint32_t k = 0; k < n; k++)
        out[k] = s->slots[k].image;
    *count = n;
    return n < s->n ? VK_INCOMPLETE : VK_SUCCESS;
}

// An image the program doesn't have: there's nothing to wait for (its
// buffer is waited for when it's presented).
static CALL VkResult CC AcquireNextImageKHR(VkDevice d, VkSwapchainKHR s, uint64_t timeout, VkSemaphore semaphore, VkFence fence, uint32_t *index) {
    if (s->lost)
        return VK_ERROR_SURFACE_LOST_KHR;
    for (uint32_t k = 0; k < s->n; k++) {
        uint32_t i = (s->next + k) % s->n;
        if (!s->slots[i].acquired) {
            s->slots[i].acquired = 1;
            s->next = i + 1;
            *index = i;
            if (fence)
                fence->signaled = 1;
            return VK_SUCCESS;
        }
    }
    return timeout ? VK_TIMEOUT : VK_NOT_READY;
}

// Presenting: once the compositor has shown the last frame (FIFO) and is
// done with this image's buffer, the image copied into it and committed.
static VkResult present(VkSwapchainKHR s, uint32_t i) {
    struct slot *sl = &s->slots[i];
    sl->acquired = 0;
    while (s->frame || sl->busy)
        if (wl.dispatch_queue(s->display, s->queue) < 0) {
            s->lost = 1;
            return VK_ERROR_SURFACE_LOST_KHR;
        }
    struct VkCommandBuffer_T c = {0};
    struct VkBuffer_T b = {s->copy.size, 0, &s->copy, 0};
    VkBufferImageCopy whole = {0, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {s->width, s->height, 1}};
    buffer_image(&c, IMAGE_TO_BUFFER, &b, sl->image, 1, &whole);
    struct request q = {VK_SUBMIT, {(uint64_t)(uintptr_t)c.s, c.n}};
    int failed = c.failed || call(&q);
    free(c.s);
    free(c.writes);
    s->copy.map = s->pixels + (size_t)s->copy.size * i;
    if (failed || transfer(READ, &s->copy, 0, s->copy.size))
        return VK_ERROR_DEVICE_LOST;
    s->frame = wl.marshal_flags(s->surface, 3, wl.callback, wl.get_version(s->surface), 0, NULL);
    if (s->frame)
        wl.add_listener(s->frame, &frame_listener, s);
    uint32_t v = wl.get_version(s->surface);
    wl.marshal_flags(s->surface, 1, NULL, v, 0, sl->buffer, 0, 0);
    if (v >= 4)
        wl.marshal_flags(s->surface, 9, NULL, v, 0, 0, 0, (int32_t)s->width, (int32_t)s->height);
    else
        wl.marshal_flags(s->surface, 2, NULL, v, 0, 0, 0, 0x7fffffff, 0x7fffffff);
    wl.marshal_flags(s->surface, 6, NULL, v, 0);
    sl->busy = 1;
    if (wl.flush(s->display) < 0) {
        s->lost = 1;
        return VK_ERROR_SURFACE_LOST_KHR;
    }
    return VK_SUCCESS;
}

static CALL VkResult CC QueuePresentKHR(VkQueue queue, const VkPresentInfoKHR *info) {
    VkResult result = VK_SUCCESS;
    for (uint32_t k = 0; k < info->swapchainCount; k++) {
        VkResult r = present(info->pSwapchains[k], info->pImageIndices[k]);
        if (info->pResults)
            info->pResults[k] = r;
        if (r != VK_SUCCESS)
            result = r;
    }
    return result;
}

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
      F(GetPhysicalDeviceSurfaceSupportKHR),
      F(GetPhysicalDeviceSurfaceCapabilitiesKHR),
      F(GetPhysicalDeviceSurfaceFormatsKHR),
      F(GetPhysicalDeviceSurfacePresentModesKHR),
      F(GetPhysicalDeviceWaylandPresentationSupportKHR),
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
      F(CreateImage),
      F(DestroyImage),
      F(GetImageMemoryRequirements),
      F(BindImageMemory),
      F(GetImageSubresourceLayout),
      F(GetImageSparseMemoryRequirements),
      F(CreateImageView),
      F(DestroyImageView),
      F(CreateSampler),
      F(DestroySampler),
      F(CreateShaderModule),
      F(DestroyShaderModule),
      F(CreateComputePipelines),
      F(CreateGraphicsPipelines),
      F(DestroyPipeline),
      F(CreateRenderPass),
      F(DestroyRenderPass),
      F(GetRenderAreaGranularity),
      F(CreateFramebuffer),
      F(DestroyFramebuffer),
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
      F(CmdCopyBufferToImage),
      F(CmdCopyImageToBuffer),
      F(CmdCopyImage),
      F(CmdClearColorImage),
      F(CmdBeginRenderPass),
      F(CmdNextSubpass),
      F(CmdEndRenderPass),
      F(CmdSetViewport),
      F(CmdSetScissor),
      F(CmdSetBlendConstants),
      F(CmdSetStencilReference),
      F(CmdSetLineWidth),
      F(CmdSetDepthBias),
      F(CmdSetDepthBounds),
      F(CmdSetStencilCompareMask),
      F(CmdSetStencilWriteMask),
      F(CmdBindVertexBuffers),
      F(CmdBindIndexBuffer),
      F(CmdDraw),
      F(CmdDrawIndexed),
      F(CmdDrawIndirect),
      F(CmdDrawIndexedIndirect),
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
      F(CreateSwapchainKHR),
      F(DestroySwapchainKHR),
      F(GetSwapchainImagesKHR),
      F(AcquireNextImageKHR),
      F(QueuePresentKHR),
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
