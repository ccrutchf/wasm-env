// wasm-env's Vulkan driver: an installable client driver (ICD) for the
// Khronos loader (libvulkan.so.1), implementing a portable subset of
// Vulkan 1.0 on the runtime's GPU service through /dev/wasm-env-gpu
// (runtime/src/gpu.rs has the requests, gpu/src/vulkan.rs the shaders and
// the commands): memory, buffers, images, SPIR-V shader modules, descriptor
// sets, compute and graphics pipelines, render passes, command buffers
// (recorded as a stream of commands the runtime replays), fences, and
// presenting to Wayland surfaces. One physical device, with one queue. Its
// entry points are generated from the Vulkan registry (entries.awk), so a
// command it doesn't implement is a stub that says so.
//
// Device memory is a buffer in the runtime. Memory the program maps is a
// copy in its own memory: sent to the device before each submission, and
// after a wait (a fence, the queue, the device) updated where the device
// may have written (what HOST_COHERENT promises; the program's own writes
// elsewhere in it stay). A submission while device writes to mapped memory
// haven't been read back reads them first, so it doesn't overwrite them.
//
// Like the OpenCL driver, it uses nothing from the C library but
// allocation, a few string functions, open, ioctl and close (and, to
// present, mapping memory and libwayland-client through dlopen): the same
// library serves glibc and musl programs.
#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>
// (Wayland's types, without wayland-client.h: pointers to its structs.)
struct wl_display;
struct wl_surface;
#include <vulkan/vulkan_wayland.h>
#define VK_USE_PLATFORM_WAYLAND_KHR
#include <vulkan/vk_icd.h>
#include <stddef.h>
#include <stdint.h>

void *malloc(size_t);
void *calloc(size_t, size_t);
void qsort(void *, size_t, size_t, int (*)(const void *, const void *));
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

enum { INFO = 1, CREATE, FREE, WRITE, READ, FINISH = 13, VK_SHADER = 20, VK_PIPELINE, VK_SUBMIT, VK_LIMITS, VK_IMAGE, VK_VIEW, VK_SAMPLER, VK_FORMAT, VK_GRAPHICS, VK_BUFFER_VIEW, VK_QUERY_POOL, VK_QUERY_RESULTS, VK_SEND };
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
    BIND_GRAPHICS_SET,
    BLIT,
    CLEAR_DEPTH,
    RESOLVE,
    CLEAR_ATTACHMENTS,
    BEGIN_QUERY,
    END_QUERY,
    RESET_QUERIES,
    TIMESTAMP,
    COPY_QUERIES,
    SET_DEPTH_BIAS,
    SET_STENCIL_COMPARE_MASK,
    SET_STENCIL_WRITE_MASK,
    FILL_TEXELS,
    BARRIER
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
    L_TIMESTAMP,
    L_FEATURES,
    L_SAMPLERS,
    L_SAMPLED_IMAGES,
    L_STORAGE_IMAGES,
    L_ATTRIBUTES,
    L_VERTEX_BUFFERS,
    L_VERTEX_STRIDE,
    L_INTER_STAGE,
    L_COLORS,
    L_DIM_1D,
    L_DIM_2D,
    L_DIM_3D,
    L_LAYERS,
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
    // The extensions enabled (bits: device_extensions's order).
    uint32_t extensions;
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
enum { S_VIEWPORT = 1, S_SCISSOR = 2, S_BLEND = 4, S_REFERENCE = 8, S_BIAS = 16, S_COMPARE_MASK = 32, S_WRITE_MASK = 64 };
struct state {
    uint32_t set;
    float viewport[6];
    uint32_t scissor[4];
    float blend[4];
    uint32_t reference;
    // Depth bias (constant, clamp, slope), stencil compare and write masks.
    float bias[3];
    uint32_t compare_mask, write_mask;
};
struct VkPipeline_T {
    uint64_t handle;
    int graphics;
    // The bindings it writes (storage buffers, texel buffers and images):
    // bits, set × 16 + binding (gpu.rs's VK_PIPELINE).
    uint64_t written;
    // A graphics pipeline's own state (its `set` what isn't dynamic).
    struct state fixed;
};
struct subpass {
    uint32_t ncolors;
    uint32_t colors[8];
    // Each color attachment's resolve attachment (VK_ATTACHMENT_UNUSED: none).
    uint32_t resolves[8];
    uint32_t depth; // VK_ATTACHMENT_UNUSED: none
    // Its views (multiview's view mask; 0: none): drawn once for each.
    uint32_t views;
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
    uint32_t n, width, height;
    VkImageView *views;
};
struct VkPipelineLayout_T {
    uint32_t sets;
    VkDescriptorSetLayout layouts[8];
};
struct VkDescriptorSetLayout_T {
    uint32_t n;
    VkDescriptorSetLayoutBinding *bindings;
    // Per binding, in binding order: its first descriptor in a set's (an
    // array's elements follow); and how many a set has.
    uint32_t *first, total;
    // Per descriptor: its immutable sampler (or NULL).
    VkSampler *immutable;
};
struct VkImage_T {
    uint64_t handle;
    VkImageType type;
    VkFormat format;
    VkExtent3D extent;
    uint32_t mips, layers, texel;
    // A linear image (VK_IMAGE_TILING_LINEAR): no texture, but the memory
    // bound to it, as a buffer of its texels, row after row.
    int linear;
    struct VkBuffer_T memory;
};
struct VkImageView_T {
    uint64_t handle;
};
struct VkBufferView_T {
    uint64_t handle;
    // Its buffer and range (a storage texel buffer's writes reach them).
    VkBuffer buffer;
    VkDeviceSize offset, range;
};
struct VkQueryPool_T {
    uint64_t handle;
    uint32_t count;
};
struct VkSampler_T {
    uint64_t handle;
};
// A descriptor: a buffer's range, an image view, a sampler, or a view and
// sampler.
struct descriptor {
    VkDescriptorType type;
    // Its binding, and element of an array.
    uint32_t binding, element;
    VkBuffer buffer;
    VkDeviceSize offset, range;
    VkImageView view;
    VkSampler sampler;
    VkBufferView texel;
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
    // And the ranges its commands may read (sent from mapped memory as it's
    // submitted).
    struct write *reads;
    size_t nreads, rcap;
    // Per bind point (graphics, compute), the pipeline bound, and each
    // set's storage descriptors bound: what a draw or dispatch writes, if
    // its pipeline writes their bindings.
    VkPipeline bound[2];
    struct stored {
        uint32_t binding;
        VkBuffer buffer;
        VkDeviceSize offset, range;
    } *stores[2][8];
    uint32_t nstores[2][8], scap[2][8];
    int failed;
    // Graphics: the state set, and the render pass begun (or NULL), its
    // framebuffer, subpass and clear values.
    struct state state;
    VkRenderPass pass;
    VkFramebuffer framebuffer;
    // The render pass's render area.
    VkRect2D area;
    uint32_t subpass;
    VkClearValue *clears;
    uint32_t nclears;
    // The events it sets and resets, in order: set (or reset) as it's
    // submitted, the host seeing them after a wait.
    struct signal {
        VkEvent event;
        int set;
    } *signals;
    size_t nsignals, sigcap;
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

// (Besides Vulkan 1.1's own: those of its extensions it has whole, for
// programs written for 1.0.)
static const VkExtensionProperties instance_extensions[] = {
    {VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_EXTENSION_NAME, VK_KHR_GET_PHYSICAL_DEVICE_PROPERTIES_2_SPEC_VERSION},
    {VK_KHR_EXTERNAL_MEMORY_CAPABILITIES_EXTENSION_NAME, VK_KHR_EXTERNAL_MEMORY_CAPABILITIES_SPEC_VERSION},
    {VK_KHR_EXTERNAL_SEMAPHORE_CAPABILITIES_EXTENSION_NAME, VK_KHR_EXTERNAL_SEMAPHORE_CAPABILITIES_SPEC_VERSION},
    {VK_KHR_EXTERNAL_FENCE_CAPABILITIES_EXTENSION_NAME, VK_KHR_EXTERNAL_FENCE_CAPABILITIES_SPEC_VERSION},
    {VK_KHR_SURFACE_EXTENSION_NAME, VK_KHR_SURFACE_SPEC_VERSION},
    {VK_KHR_WAYLAND_SURFACE_EXTENSION_NAME, VK_KHR_WAYLAND_SURFACE_SPEC_VERSION},
};
static const VkExtensionProperties device_extensions[] = {
    {VK_KHR_SWAPCHAIN_EXTENSION_NAME, VK_KHR_SWAPCHAIN_SPEC_VERSION},
    {VK_KHR_MAINTENANCE_1_EXTENSION_NAME, VK_KHR_MAINTENANCE_1_SPEC_VERSION},
    {VK_KHR_MAINTENANCE_2_EXTENSION_NAME, VK_KHR_MAINTENANCE_2_SPEC_VERSION},
    {VK_KHR_MAINTENANCE_3_EXTENSION_NAME, VK_KHR_MAINTENANCE_3_SPEC_VERSION},
    {VK_KHR_GET_MEMORY_REQUIREMENTS_2_EXTENSION_NAME, VK_KHR_GET_MEMORY_REQUIREMENTS_2_SPEC_VERSION},
    {VK_KHR_DEDICATED_ALLOCATION_EXTENSION_NAME, VK_KHR_DEDICATED_ALLOCATION_SPEC_VERSION},
    {VK_KHR_BIND_MEMORY_2_EXTENSION_NAME, VK_KHR_BIND_MEMORY_2_SPEC_VERSION},
    {VK_KHR_DESCRIPTOR_UPDATE_TEMPLATE_EXTENSION_NAME, VK_KHR_DESCRIPTOR_UPDATE_TEMPLATE_SPEC_VERSION},
    {VK_KHR_MULTIVIEW_EXTENSION_NAME, VK_KHR_MULTIVIEW_SPEC_VERSION},
    {VK_KHR_STORAGE_BUFFER_STORAGE_CLASS_EXTENSION_NAME, VK_KHR_STORAGE_BUFFER_STORAGE_CLASS_SPEC_VERSION},
    {VK_KHR_RELAXED_BLOCK_LAYOUT_EXTENSION_NAME, VK_KHR_RELAXED_BLOCK_LAYOUT_SPEC_VERSION},
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
    *version = VK_API_VERSION_1_1;
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
    p->apiVersion = VK_API_VERSION_1_1;
    p->driverVersion = VK_MAKE_VERSION(0, 1, 0);
    p->vendorID = (uint32_t)limits[L_VENDOR];
    p->deviceID = (uint32_t)limits[L_DEVICE];
    p->deviceType = (VkPhysicalDeviceType)limits[L_TYPE];
    snprintf(p->deviceName, sizeof p->deviceName, "%s (wasm-env)", device_name);
    memcpy(p->pipelineCacheUUID, "wasm-env-vulkan", 16);
    VkPhysicalDeviceLimits *l = &p->limits;
    // The device's limits (WebGPU's), each at least Vulkan's minimum.
#define AT_LEAST(v, min) ((uint32_t)((v) > (min) ? (v) : (min)))
    uint32_t dim2 = AT_LEAST(limits[L_DIM_2D], 4096);
    // (A 1D image is a 2D one's row.)
    l->maxImageDimension1D = AT_LEAST(limits[L_DIM_2D], 4096);
    l->maxImageDimension2D = l->maxImageDimensionCube = dim2;
    l->maxImageDimension3D = AT_LEAST(limits[L_DIM_3D], 256);
    l->maxImageArrayLayers = AT_LEAST(limits[L_LAYERS], 256);
    // (A texel buffer is a 2D texture 4096 texels wide: vulkan.rs.)
    l->maxTexelBufferElements = AT_LEAST(limits[L_DIM_2D] * 4096, 65536);
    l->maxUniformBufferRange = (uint32_t)(limits[L_UNIFORM_RANGE] < 0xffffffffu ? limits[L_UNIFORM_RANGE] : 0xffffffffu);
    l->maxStorageBufferRange = (uint32_t)(limits[L_STORAGE_RANGE] < 0xffffffffu ? limits[L_STORAGE_RANGE] : 0xffffffffu);
    l->maxPushConstantsSize = (uint32_t)limits[L_PUSH];
    l->maxMemoryAllocationCount = 4096;
    l->maxSamplerAllocationCount = 4000;
    l->bufferImageGranularity = 1;
    l->maxBoundDescriptorSets = (uint32_t)limits[L_SETS];
    // (Per stage the device's; per set, as many as every stage has.)
    uint32_t samplers = AT_LEAST(limits[L_SAMPLERS], 16), sampled = AT_LEAST(limits[L_SAMPLED_IMAGES], 16), storage = AT_LEAST(limits[L_STORAGE_IMAGES], 4);
    uint32_t uniform = AT_LEAST(limits[L_UNIFORM_BUFFERS], 12), storage_buffers = AT_LEAST(limits[L_STORAGE_BUFFERS], 4);
    l->maxPerStageDescriptorSamplers = samplers;
    l->maxPerStageDescriptorUniformBuffers = uniform;
    l->maxPerStageDescriptorStorageBuffers = storage_buffers;
    l->maxPerStageDescriptorSampledImages = sampled;
    l->maxPerStageDescriptorStorageImages = storage;
    l->maxPerStageDescriptorInputAttachments = AT_LEAST(sampled < 8 ? sampled : 8, 4);
    l->maxPerStageResources = AT_LEAST(uniform + storage_buffers + sampled + storage, 128);
    l->maxDescriptorSetSamplers = AT_LEAST(6 * samplers, 96);
    l->maxDescriptorSetUniformBuffers = AT_LEAST(6 * uniform, 72);
    l->maxDescriptorSetUniformBuffersDynamic = 8;
    l->maxDescriptorSetStorageBuffers = AT_LEAST(6 * storage_buffers, 24);
    l->maxDescriptorSetStorageBuffersDynamic = 4;
    l->maxDescriptorSetSampledImages = AT_LEAST(6 * sampled, 96);
    l->maxDescriptorSetStorageImages = AT_LEAST(6 * storage, 24);
    l->maxDescriptorSetInputAttachments = l->maxPerStageDescriptorInputAttachments;
    // Vertex input and stages' interfaces.
    l->maxVertexInputAttributes = AT_LEAST(limits[L_ATTRIBUTES], 16);
    l->maxVertexInputBindings = AT_LEAST(limits[L_VERTEX_BUFFERS], 16);
    l->maxVertexInputBindingStride = AT_LEAST(limits[L_VERTEX_STRIDE], 2048);
    l->maxVertexInputAttributeOffset = l->maxVertexInputBindingStride - 1;
    l->maxVertexOutputComponents = l->maxFragmentInputComponents = AT_LEAST(4 * limits[L_INTER_STAGE], 64);
    l->maxFragmentOutputAttachments = l->maxColorAttachments = AT_LEAST(limits[L_COLORS], 4);
    l->maxFragmentDualSrcAttachments = 0;
    l->maxFragmentCombinedOutputResources = l->maxColorAttachments + storage_buffers + storage;
    l->maxComputeSharedMemorySize = (uint32_t)limits[L_SHARED];
    for (int k = 0; k < 3; k++)
        l->maxComputeWorkGroupCount[k] = (uint32_t)limits[L_GROUPS];
    l->maxComputeWorkGroupInvocations = (uint32_t)limits[L_INVOCATIONS];
    l->maxComputeWorkGroupSize[0] = (uint32_t)limits[L_SIZE_X];
    l->maxComputeWorkGroupSize[1] = (uint32_t)limits[L_SIZE_Y];
    l->maxComputeWorkGroupSize[2] = (uint32_t)limits[L_SIZE_Z];
    // (WebGPU's: 32-bit indices, indirect draws looped over, texel offsets
    // of -8 to 7, and precision as Vulkan's minimums.)
    l->maxDrawIndexedIndexValue = 0xffffffffu;
    l->maxDrawIndirectCount = 0xffffffffu;
    l->minTexelOffset = l->minTexelGatherOffset = -8;
    l->maxTexelOffset = l->maxTexelGatherOffset = 7;
    l->minInterpolationOffset = -0.5f;
    l->maxInterpolationOffset = 0.4375f;
    l->subPixelInterpolationOffsetBits = 4;
    l->subPixelPrecisionBits = l->subTexelPrecisionBits = l->mipmapPrecisionBits = 4;
    l->maxSamplerLodBias = 2;
    l->maxClipDistances = l->maxCullDistances = l->maxCombinedClipAndCullDistances = 0;
    l->maxSampleMaskWords = 1;
    l->discreteQueuePriorities = 2;
    // (Points and lines of one pixel: no large points or wide lines.)
    l->pointSizeRange[0] = l->pointSizeRange[1] = 1;
    l->lineWidthRange[0] = l->lineWidthRange[1] = 1;
    l->pointSizeGranularity = l->lineWidthGranularity = 0;
    l->strictLines = VK_FALSE;
    l->standardSampleLocations = VK_TRUE;
    l->minMemoryMapAlignment = 64;
    l->minTexelBufferOffsetAlignment = 256;
    l->minUniformBufferOffsetAlignment = limits[L_UNIFORM_ALIGN];
    l->minStorageBufferOffsetAlignment = limits[L_STORAGE_ALIGN];
    l->maxViewports = 1;
    l->maxViewportDimensions[0] = l->maxViewportDimensions[1] = dim2;
    l->viewportBoundsRange[0] = -2.0f * dim2;
    l->viewportBoundsRange[1] = 2.0f * dim2 - 1;
    l->viewportSubPixelBits = 0;
    l->maxFramebufferWidth = l->maxFramebufferHeight = dim2;
    l->maxFramebufferLayers = l->maxImageArrayLayers;
    // (One sample, or four: WebGPU's.)
    l->framebufferColorSampleCounts = l->framebufferDepthSampleCounts = l->framebufferStencilSampleCounts = VK_SAMPLE_COUNT_1_BIT | VK_SAMPLE_COUNT_4_BIT;
    l->framebufferNoAttachmentsSampleCounts = VK_SAMPLE_COUNT_1_BIT | VK_SAMPLE_COUNT_4_BIT;
    l->sampledImageColorSampleCounts = l->sampledImageIntegerSampleCounts = VK_SAMPLE_COUNT_1_BIT | VK_SAMPLE_COUNT_4_BIT;
    l->sampledImageDepthSampleCounts = l->sampledImageStencilSampleCounts = VK_SAMPLE_COUNT_1_BIT | VK_SAMPLE_COUNT_4_BIT;
    l->storageImageSampleCounts = VK_SAMPLE_COUNT_1_BIT;
    l->maxSamplerAnisotropy = 16;
    // (Timestamps where the device takes them: their period.)
    uint32_t period = (uint32_t)limits[L_TIMESTAMP];
    l->timestampComputeAndGraphics = period != 0;
    memcpy(&l->timestampPeriod, &period, 4);
    if (!period)
        l->timestampPeriod = 1;
    l->optimalBufferCopyOffsetAlignment = 4;
    l->optimalBufferCopyRowPitchAlignment = 256;
    l->nonCoherentAtomSize = 4;
}

// The optional features the driver has: those every device has through it,
// and those the device has (VK_LIMITS's features).
static VkPhysicalDeviceFeatures supported(void) {
    uint64_t has = limits[L_FEATURES];
    VkPhysicalDeviceFeatures f = {0};
    // (WebGPU checks buffer accesses; 32-bit indices, cube arrays, blends of
    // their own per attachment, indirect draws' counts looped over, depth
    // bias clamped, anisotropic filtering, fragment shaders' stores.)
    f.robustBufferAccess = f.fullDrawIndexUint32 = f.imageCubeArray = f.independentBlend = VK_TRUE;
    f.multiDrawIndirect = f.depthBiasClamp = f.samplerAnisotropy = f.fragmentStoresAndAtomics = VK_TRUE;
    f.shaderSampledImageArrayDynamicIndexing = f.shaderStorageBufferArrayDynamicIndexing = !!(has & 1);
    f.shaderFloat64 = !!(has & 2);
    f.shaderInt64 = !!(has & 4);
    f.drawIndirectFirstInstance = !!(has & 8);
    f.textureCompressionBC = !!(has & 16);
    return f;
}

static CALL void CC GetPhysicalDeviceFeatures(VkPhysicalDevice pd, VkPhysicalDeviceFeatures *f) {
    *f = supported();
}

// The one queue family (its timestamps 64 bits, if the device takes them).
static VkQueueFamilyProperties queue_family(void) {
    VkQueueFamilyProperties f = {VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT, 1, limits[L_TIMESTAMP] ? 64 : 0, {1, 1, 1}};
    return f;
}

static CALL void CC GetPhysicalDeviceQueueFamilyProperties(VkPhysicalDevice pd, uint32_t *count, VkQueueFamilyProperties *out) {
    VkQueueFamilyProperties family = queue_family();
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
static VkFormatFeatureFlags format_samples(VkFormat f, uint32_t *texel, VkSampleCountFlags *samples) {
    struct request q = {VK_FORMAT, {(uint64_t)f}};
    if (call(&q))
        q.r[0] = q.r[2] = q.r[3] = 0;
    if (texel)
        *texel = (uint32_t)q.r[2];
    if (samples)
        *samples = (VkSampleCountFlags)q.r[3];
    // (Images' features: a vertex attribute's is a buffer's, below.)
    return (VkFormatFeatureFlags)q.r[0] & ~VK_FORMAT_FEATURE_VERTEX_BUFFER_BIT;
}

// A format's features in buffers: vertex attributes, and texel buffers
// where images of it can be sampled or stored (2D textures of their
// texels: vulkan.rs).
static VkFormatFeatureFlags buffer_features(VkFormat f) {
    struct request q = {VK_FORMAT, {(uint64_t)f}};
    if (call(&q))
        return 0;
    VkFormatFeatureFlags have = (VkFormatFeatureFlags)q.r[0], b = have & VK_FORMAT_FEATURE_VERTEX_BUFFER_BIT;
    int blocks = f >= VK_FORMAT_BC1_RGB_UNORM_BLOCK && f <= VK_FORMAT_ASTC_12x12_SRGB_BLOCK;
    if (q.r[2] && !blocks && !(have & VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT)) {
        if (have & VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT)
            b |= VK_FORMAT_FEATURE_UNIFORM_TEXEL_BUFFER_BIT;
        if (have & VK_FORMAT_FEATURE_STORAGE_IMAGE_BIT)
            b |= VK_FORMAT_FEATURE_STORAGE_TEXEL_BUFFER_BIT;
    }
    return b;
}

// A format's features (VkFormatFeatureFlags), and the bytes of a texel.
static VkFormatFeatureFlags format_features(VkFormat f, uint32_t *texel) {
    return format_samples(f, texel, NULL);
}

// The features a format has in linear images: copies, where its texels
// are whole bytes of color (not depth or blocks).
static VkFormatFeatureFlags linear_features(VkFormat f) {
    uint32_t texel;
    VkFormatFeatureFlags have = format_features(f, &texel);
    int blocks = f >= VK_FORMAT_BC1_RGB_UNORM_BLOCK && f <= VK_FORMAT_ASTC_12x12_SRGB_BLOCK;
    if (!texel || blocks || (have & VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT))
        return 0;
    return have & (VK_FORMAT_FEATURE_TRANSFER_SRC_BIT | VK_FORMAT_FEATURE_TRANSFER_DST_BIT);
}

// (Optimal images are textures; linear ones are memory, which only
// copies reach.)
static CALL void CC GetPhysicalDeviceFormatProperties(VkPhysicalDevice pd, VkFormat f, VkFormatProperties *p) {
    memset(p, 0, sizeof *p);
    p->optimalTilingFeatures = format_features(f, NULL);
    p->linearTilingFeatures = linear_features(f);
    p->bufferFeatures = buffer_features(f);
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

static VkResult image_format_properties(VkFormat f, VkImageType t, VkImageTiling tl, VkImageUsageFlags u, VkImageCreateFlags c, VkImageFormatProperties *p);

// (Not supported: the properties all zeros, as Vulkan says.)
static CALL VkResult CC GetPhysicalDeviceImageFormatProperties(VkPhysicalDevice pd, VkFormat f, VkImageType t, VkImageTiling tl, VkImageUsageFlags u, VkImageCreateFlags c, VkImageFormatProperties *p) {
    VkResult r = image_format_properties(f, t, tl, u, c, p);
    if (r != VK_SUCCESS)
        memset(p, 0, sizeof *p);
    return r;
}

static VkResult image_format_properties(VkFormat f, VkImageType t, VkImageTiling tl, VkImageUsageFlags u, VkImageCreateFlags c, VkImageFormatProperties *p) {
    if (tl == VK_IMAGE_TILING_LINEAR) {
        // (Linear: one level and layer, copied to and from.)
        VkImageUsageFlags copies = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
        if (!linear_features(f) || (u & ~copies) || c)
            return VK_ERROR_FORMAT_NOT_SUPPORTED;
        memset(p, 0, sizeof *p);
        uint32_t d2 = AT_LEAST(limits[L_DIM_2D], 4096), d3 = AT_LEAST(limits[L_DIM_3D], 256);
        p->maxExtent = t == VK_IMAGE_TYPE_3D ? (VkExtent3D){d3, d3, d3} : t == VK_IMAGE_TYPE_1D ? (VkExtent3D){d2, 1, 1} : (VkExtent3D){d2, d2, 1};
        p->maxMipLevels = p->maxArrayLayers = 1;
        p->sampleCounts = VK_SAMPLE_COUNT_1_BIT;
        p->maxResourceSize = (VkDeviceSize)1 << 31;
        return VK_SUCCESS;
    }
    VkSampleCountFlags samples;
    VkFormatFeatureFlags have = format_samples(f, NULL, &samples);
    // (An input attachment is an attachment's, color or depth.)
    if ((u & VK_IMAGE_USAGE_INPUT_ATTACHMENT_BIT) && !(have & (VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT | VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT)))
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    if (tl != VK_IMAGE_TILING_OPTIMAL || !have || (have & needs(u)) != needs(u) || (c & ~(VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT | VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT | VK_IMAGE_CREATE_EXTENDED_USAGE_BIT)))
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    memset(p, 0, sizeof *p);
    uint32_t d1 = AT_LEAST(limits[L_DIM_2D], 4096), d2 = AT_LEAST(limits[L_DIM_2D], 4096), d3 = AT_LEAST(limits[L_DIM_3D], 256);
    p->maxExtent = t == VK_IMAGE_TYPE_3D ? (VkExtent3D){d3, d3, d3} : t == VK_IMAGE_TYPE_1D ? (VkExtent3D){d1, 1, 1} : (VkExtent3D){d2, d2, 1};
    uint32_t largest = t == VK_IMAGE_TYPE_3D ? d3 : t == VK_IMAGE_TYPE_1D ? d1 : d2;
    p->maxMipLevels = 1;
    while (largest >>= 1)
        p->maxMipLevels++;
    p->maxArrayLayers = t == VK_IMAGE_TYPE_3D ? 1 : AT_LEAST(limits[L_LAYERS], 256);
    // (A 1D image is 2D, a row high: of no blocks, which a row isn't of.
    // WebGPU's 3D images are of neither depth nor blocks.)
    int depth = have & VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT;
    int blocks = f >= VK_FORMAT_BC1_RGB_UNORM_BLOCK && f <= VK_FORMAT_ASTC_12x12_SRGB_BLOCK;
    if (t == VK_IMAGE_TYPE_1D && blocks)
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    if (t == VK_IMAGE_TYPE_3D && (depth || blocks))
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    p->sampleCounts = VK_SAMPLE_COUNT_1_BIT;
    // (Four samples, as WebGPU has them: 2D attachments, one level and
    // layer, of formats it multisamples; not storage.)
    if (t == VK_IMAGE_TYPE_2D && (samples & VK_SAMPLE_COUNT_4_BIT) && !(u & VK_IMAGE_USAGE_STORAGE_BIT) && !(c & VK_IMAGE_CREATE_CUBE_COMPATIBLE_BIT) &&
        (have & (VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT | VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT)))
        p->sampleCounts |= VK_SAMPLE_COUNT_4_BIT;
    p->maxResourceSize = (VkDeviceSize)1 << 31;
    return VK_SUCCESS;
}

static CALL void CC GetPhysicalDeviceSparseImageFormatProperties(VkPhysicalDevice pd, VkFormat f, VkImageType t, VkSampleCountFlagBits s, VkImageUsageFlags u, VkImageTiling tl, uint32_t *count, VkSparseImageFormatProperties *p) {
    *count = 0;
}

// The ...2 forms (VK_KHR_get_physical_device_properties2, core in 1.1):
// the 1.0 ones, and Vulkan 1.1's structures in their chains.
static CALL void CC GetPhysicalDeviceProperties2(VkPhysicalDevice pd, VkPhysicalDeviceProperties2 *p) {
    GetPhysicalDeviceProperties(pd, &p->properties);
    for (VkBaseOutStructure *s = p->pNext; s; s = s->pNext)
        switch (s->sType) {
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES: {
            VkPhysicalDeviceIDProperties *id = (VkPhysicalDeviceIDProperties *)s;
            memset(id->deviceUUID, 0, VK_UUID_SIZE);
            memcpy(id->deviceUUID, "wasm-env", 8);
            memcpy(id->deviceUUID + 8, &limits[L_VENDOR], 4);
            memcpy(id->deviceUUID + 12, &limits[L_DEVICE], 4);
            memcpy(id->driverUUID, "wasm-env-vulkan", 16);
            memset(id->deviceLUID, 0, VK_LUID_SIZE);
            id->deviceNodeMask = 0;
            id->deviceLUIDValid = VK_FALSE;
            break;
        }
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_3_PROPERTIES: {
            VkPhysicalDeviceMaintenance3Properties *m = (VkPhysicalDeviceMaintenance3Properties *)s;
            m->maxPerSetDescriptors = 1024;
            m->maxMemoryAllocationSize = limits[L_ALLOCATION];
            break;
        }
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTIVIEW_PROPERTIES: {
            // (Each view a pass of its own, of its layer: as many as a
            // view mask has bits.)
            VkPhysicalDeviceMultiviewProperties *m = (VkPhysicalDeviceMultiviewProperties *)s;
            m->maxMultiviewViewCount = 32;
            m->maxMultiviewInstanceIndex = (1u << 27) - 1;
            break;
        }
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_POINT_CLIPPING_PROPERTIES:
            ((VkPhysicalDevicePointClippingProperties *)s)->pointClippingBehavior = VK_POINT_CLIPPING_BEHAVIOR_ALL_CLIP_PLANES;
            break;
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_PROPERTIES:
            ((VkPhysicalDeviceProtectedMemoryProperties *)s)->protectedNoFault = VK_FALSE;
            break;
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_PROPERTIES: {
            // (Subgroups of one invocation, as the SPIR-V is made to have
            // them on any device: gpu/src/spirv.rs.)
            VkPhysicalDeviceSubgroupProperties *g = (VkPhysicalDeviceSubgroupProperties *)s;
            g->subgroupSize = 1;
            g->supportedStages = VK_SHADER_STAGE_VERTEX_BIT | VK_SHADER_STAGE_FRAGMENT_BIT | VK_SHADER_STAGE_COMPUTE_BIT;
            g->supportedOperations = VK_SUBGROUP_FEATURE_BASIC_BIT | VK_SUBGROUP_FEATURE_VOTE_BIT;
            g->quadOperationsInAllStages = VK_FALSE;
            break;
        }
        default:
            break;
        }
}
static CALL void CC GetPhysicalDeviceFeatures2(VkPhysicalDevice pd, VkPhysicalDeviceFeatures2 *f) {
    GetPhysicalDeviceFeatures(pd, &f->features);
    // (Of 1.1's optional features, multiview, its only required one.)
    for (VkBaseOutStructure *s = f->pNext; s; s = s->pNext)
        switch (s->sType) {
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_16BIT_STORAGE_FEATURES:
            memset((char *)s + sizeof(VkBaseOutStructure), 0, sizeof(VkPhysicalDevice16BitStorageFeatures) - sizeof(VkBaseOutStructure));
            break;
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTIVIEW_FEATURES: {
            VkPhysicalDeviceMultiviewFeatures *m = (VkPhysicalDeviceMultiviewFeatures *)s;
            m->multiview = VK_TRUE;
            m->multiviewGeometryShader = m->multiviewTessellationShader = VK_FALSE;
            break;
        }
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_FEATURES:
            ((VkPhysicalDeviceProtectedMemoryFeatures *)s)->protectedMemory = VK_FALSE;
            break;
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SAMPLER_YCBCR_CONVERSION_FEATURES:
            ((VkPhysicalDeviceSamplerYcbcrConversionFeatures *)s)->samplerYcbcrConversion = VK_FALSE;
            break;
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SHADER_DRAW_PARAMETERS_FEATURES:
            ((VkPhysicalDeviceShaderDrawParametersFeatures *)s)->shaderDrawParameters = VK_FALSE;
            break;
        case VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VARIABLE_POINTERS_FEATURES: {
            VkPhysicalDeviceVariablePointersFeatures *v = (VkPhysicalDeviceVariablePointersFeatures *)s;
            v->variablePointersStorageBuffer = v->variablePointers = VK_FALSE;
            break;
        }
        default:
            break;
        }
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
        out[0].queueFamilyProperties = queue_family();
    *count = *count < 1 ? *count : 1;
}
static CALL VkResult CC GetPhysicalDeviceImageFormatProperties2(VkPhysicalDevice pd, const VkPhysicalDeviceImageFormatInfo2 *i, VkImageFormatProperties2 *p) {
    // (No memory shared with other APIs or processes.)
    for (const VkBaseInStructure *s = i->pNext; s; s = s->pNext)
        if (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO && ((const VkPhysicalDeviceExternalImageFormatInfo *)s)->handleType)
            return VK_ERROR_FORMAT_NOT_SUPPORTED;
    for (VkBaseOutStructure *s = p->pNext; s; s = s->pNext) {
        if (s->sType == VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES)
            memset(&((VkExternalImageFormatProperties *)s)->externalMemoryProperties, 0, sizeof(VkExternalMemoryProperties));
        if (s->sType == VK_STRUCTURE_TYPE_SAMPLER_YCBCR_CONVERSION_IMAGE_FORMAT_PROPERTIES)
            ((VkSamplerYcbcrConversionImageFormatProperties *)s)->combinedImageSamplerDescriptorCount = 1;
    }
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
        // (Asking for a feature it hasn't fails.)
        VkPhysicalDeviceFeatures have = supported();
        const VkBool32 *f = (const VkBool32 *)info->pEnabledFeatures, *h = (const VkBool32 *)&have;
        for (size_t k = 0; k < sizeof(VkPhysicalDeviceFeatures) / sizeof(VkBool32); k++)
            if (f[k] && !h[k])
                return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    // (Or asked for in a VkPhysicalDeviceFeatures2, or 1.1's structures:
    // of those, only multiview, without geometry or tessellation.)
    for (const VkBaseInStructure *s = info->pNext; s; s = s->pNext) {
        if (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MULTIVIEW_FEATURES) {
            const VkPhysicalDeviceMultiviewFeatures *m = (const VkPhysicalDeviceMultiviewFeatures *)s;
            if (m->multiviewGeometryShader || m->multiviewTessellationShader)
                return VK_ERROR_FEATURE_NOT_PRESENT;
        }
        if (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_16BIT_STORAGE_FEATURES) {
            const VkPhysicalDevice16BitStorageFeatures *b = (const VkPhysicalDevice16BitStorageFeatures *)s;
            if (b->storageBuffer16BitAccess || b->uniformAndStorageBuffer16BitAccess || b->storagePushConstant16 || b->storageInputOutput16)
                return VK_ERROR_FEATURE_NOT_PRESENT;
        }
        if ((s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROTECTED_MEMORY_FEATURES && ((const VkPhysicalDeviceProtectedMemoryFeatures *)s)->protectedMemory) ||
            (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SAMPLER_YCBCR_CONVERSION_FEATURES && ((const VkPhysicalDeviceSamplerYcbcrConversionFeatures *)s)->samplerYcbcrConversion) ||
            (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SHADER_DRAW_PARAMETERS_FEATURES && ((const VkPhysicalDeviceShaderDrawParametersFeatures *)s)->shaderDrawParameters) ||
            (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VARIABLE_POINTERS_FEATURES && ((const VkPhysicalDeviceVariablePointersFeatures *)s)->variablePointers) ||
            (s->sType == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VARIABLE_POINTERS_FEATURES && ((const VkPhysicalDeviceVariablePointersFeatures *)s)->variablePointersStorageBuffer))
            return VK_ERROR_FEATURE_NOT_PRESENT;
        if (s->sType != VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2)
            continue;
        VkPhysicalDeviceFeatures have = supported();
        const VkBool32 *f = (const VkBool32 *)&((const VkPhysicalDeviceFeatures2 *)s)->features, *h = (const VkBool32 *)&have;
        for (size_t k = 0; k < sizeof(VkPhysicalDeviceFeatures) / sizeof(VkBool32); k++)
            if (f[k] && !h[k])
                return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    struct VkDevice_T *d = zalloc(sizeof *d);
    if (!d)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    set_loader_magic_value(d);
    set_loader_magic_value(&d->queue);
    for (uint32_t i = 0; i < info->enabledExtensionCount; i++)
        for (size_t k = 0; k < sizeof device_extensions / sizeof *device_extensions; k++)
            if (!strcmp(info->ppEnabledExtensionNames[i], device_extensions[k].extensionName))
                d->extensions |= 1u << k;
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
static int by_start(const void *a, const void *b) {
    const VkDeviceSize *x = a, *y = b;
    return x[0] < y[0] ? -1 : x[0] > y[0];
}

static int catch_up(VkDeviceMemory m) {
    // (Each range once: draws note the same ones over and over.)
    qsort(m->pending, m->npending, 2 * sizeof *m->pending, by_start);
    int failed = 0;
    for (size_t k = 0; k < m->npending;) {
        VkDeviceSize start = m->pending[2 * k], end = m->pending[2 * k + 1];
        for (k++; k < m->npending && m->pending[2 * k] <= end; k++)
            if (m->pending[2 * k + 1] > end)
                end = m->pending[2 * k + 1];
        failed |= transfer(READ, m, start, end);
    }
    m->npending = 0;
    return failed;
}

static CALL VkResult CC MapMemory(VkDevice d, VkDeviceMemory m, VkDeviceSize offset, VkDeviceSize size, VkMemoryMapFlags flags, void **out) {
    if (m->map)
        return VK_ERROR_MEMORY_MAP_FAILED;
    m->map = malloc(m->size ? m->size : 1);
    if (!m->map)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    // (Read, then sent whole once: the runtime keeps what it was sent, to
    // send only what changes, VK_SEND.)
    if (transfer(READ, m, 0, m->size) || transfer(VK_SEND, m, 0, m->size)) {
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
    // (Unmapping makes the program's writes the device's: what changed.)
    catch_up(m);
    transfer(VK_SEND, m, 0, m->size);
    unlist(d, m);
    free(m->map);
    m->map = NULL;
}

static CALL VkResult CC FlushMappedMemoryRanges(VkDevice d, uint32_t n, const VkMappedMemoryRange *r) {
    for (uint32_t i = 0; i < n; i++) {
        VkDeviceMemory m = r[i].memory;
        VkDeviceSize end = r[i].size == VK_WHOLE_SIZE ? m->size : r[i].offset + r[i].size;
        if (m->map && (catch_up(m) || transfer(VK_SEND, m, r[i].offset, end)))
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
    // (Larger than memory can be: no memory would hold it.)
    if (info->size > limits[L_ALLOCATION])
        return VK_ERROR_OUT_OF_DEVICE_MEMORY;
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
    // (And a texel buffer view's offset's, minTexelBufferOffsetAlignment.)
    r->alignment = align > 256 ? align : 256;
    // (Rounded up to 16: room for a shader whose block WebGPU rounds up, a
    // bvec3's 12 bytes to 16.)
    r->size = (b->size + 15) & ~(VkDeviceSize)15;
    r->memoryTypeBits = 1;
}

static CALL VkResult CC BindBufferMemory(VkDevice d, VkBuffer b, VkDeviceMemory m, VkDeviceSize offset) {
    b->memory = m;
    b->offset = offset;
    return VK_SUCCESS;
}

// ---- Images, views and samplers ----

// An image is a texture in the runtime, not part of its memory: binding
// memory to it changes nothing. A linear one is its memory.
static CALL VkResult CC CreateImage(VkDevice d, const VkImageCreateInfo *info, const VkAllocationCallbacks *a, VkImage *out) {
    uint32_t texel;
    if (info->tiling == VK_IMAGE_TILING_LINEAR) {
        if (!linear_features(info->format) || info->mipLevels != 1 || info->arrayLayers != 1 || info->samples != VK_SAMPLE_COUNT_1_BIT)
            return VK_ERROR_FORMAT_NOT_SUPPORTED;
        format_features(info->format, &texel);
        struct VkImage_T *im = zalloc(sizeof *im);
        if (!im)
            return VK_ERROR_OUT_OF_HOST_MEMORY;
        *im = (struct VkImage_T){0, info->imageType, info->format, info->extent, 1, 1, texel, 1};
        im->memory.size = (VkDeviceSize)info->extent.width * info->extent.height * info->extent.depth * texel;
        *out = im;
        return VK_SUCCESS;
    }
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
    if (!im->linear) {
        struct request q = {FREE, {im->handle}};
        call(&q);
    }
    free(im);
}

static CALL void CC GetImageMemoryRequirements(VkDevice d, VkImage im, VkMemoryRequirements *r) {
    // (What its texels would take: the program allocates that much, which
    // the image doesn't use, but a linear one does.)
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
    im->memory.memory = m;
    im->memory.offset = offset;
    return VK_SUCCESS;
}

// (A linear image's rows are its texels, one after another.)
static CALL void CC GetImageSubresourceLayout(VkDevice d, VkImage im, const VkImageSubresource *s, VkSubresourceLayout *l) {
    memset(l, 0, sizeof *l);
    if (im->linear) {
        l->rowPitch = (VkDeviceSize)im->extent.width * im->texel;
        l->arrayPitch = l->depthPitch = l->rowPitch * im->extent.height;
        l->size = im->memory.size;
    }
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
    uint32_t w[11] = {(uint32_t)info->magFilter,
                      (uint32_t)info->minFilter,
                      (uint32_t)info->mipmapMode,
                      (uint32_t)info->addressModeU,
                      (uint32_t)info->addressModeV,
                      (uint32_t)info->addressModeW,
                      info->anisotropyEnable ? (uint32_t)info->maxAnisotropy : 1u,
                      info->compareEnable ? (uint32_t)info->compareOp + 1 : 0u,
                      lo.u,
                      hi.u,
                      (uint32_t)info->borderColor};
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

// A texel buffer's view: a 1D texture of its texels in the runtime,
// brought up to date with the buffer before each use (VK_BUFFER_VIEW).
static CALL VkResult CC CreateBufferView(VkDevice d, const VkBufferViewCreateInfo *info, const VkAllocationCallbacks *a, VkBufferView *out) {
    VkBuffer b = info->buffer;
    VkDeviceSize range = info->range == VK_WHOLE_SIZE ? b->size - info->offset : info->range;
    struct VkBufferView_T *v = zalloc(sizeof *v);
    struct request q = {VK_BUFFER_VIEW, {b->memory->handle, b->offset + info->offset, range, (uint64_t)info->format}};
    if (!v || call(&q)) {
        free(v);
        return VK_ERROR_FORMAT_NOT_SUPPORTED;
    }
    v->handle = q.r[0];
    v->buffer = b;
    v->offset = info->offset;
    v->range = range;
    *out = v;
    return VK_SUCCESS;
}

static CALL void CC DestroyBufferView(VkDevice d, VkBufferView v, const VkAllocationCallbacks *a) {
    if (!v)
        return;
    struct request q = {FREE, {v->handle}};
    call(&q);
    free(v);
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

// Specialization constants as the runtime takes them: per constant its id,
// size, and bytes (two words), at `w` (4 words each).
static void spec_words(const VkSpecializationInfo *s, uint32_t *w) {
    for (uint32_t k = 0; s && k < s->mapEntryCount; k++) {
        const VkSpecializationMapEntry *e = &s->pMapEntries[k];
        uint32_t v[2] = {0, 0};
        size_t n = e->size < 8 ? e->size : 8;
        if (e->offset + n <= s->dataSize)
            memcpy(v, (const uint8_t *)s->pData + e->offset, n);
        w[4 * k] = e->constantID, w[4 * k + 1] = (uint32_t)e->size, w[4 * k + 2] = v[0], w[4 * k + 3] = v[1];
    }
}

static CALL VkResult CC CreateComputePipelines(VkDevice d, VkPipelineCache cache, uint32_t n, const VkComputePipelineCreateInfo *info, const VkAllocationCallbacks *a, VkPipeline *out) {
    VkResult result = VK_SUCCESS;
    for (uint32_t i = 0; i < n; i++) {
        out[i] = VK_NULL_HANDLE;
        const VkPipelineShaderStageCreateInfo *st = &info[i].stage;
        char why[256];
        const VkSpecializationInfo *spec = st->pSpecializationInfo;
        uint32_t nspec = spec ? spec->mapEntryCount : 0;
        uint32_t *w = zalloc(nspec * 16);
        if (w)
            spec_words(spec, w);
        struct request q = {VK_PIPELINE, {st->module->handle, (uint64_t)(uintptr_t)st->pName, strlen(st->pName), (uint64_t)(uintptr_t)why, sizeof why, (uint64_t)(uintptr_t)w, nspec}};
        struct VkPipeline_T *p = zalloc(sizeof *p);
        int failed = !p || !w || call(&q);
        free(w);
        if (failed) {
            free(p);
            result = VK_ERROR_INITIALIZATION_FAILED;
            continue;
        }
        p->handle = q.r[0];
        p->written = q.r[2];
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
    const VkPipelineShaderStageCreateInfo *stage[2] = {vs, fs};
    for (int k = 0; k < 2; k++) {
        const VkSpecializationInfo *spec = stage[k] ? stage[k]->pSpecializationInfo : NULL;
        uint32_t nspec = spec ? spec->mapEntryCount : 0, w[4 * 8];
        word(&b, nspec);
        for (uint32_t e = 0; e < nspec; e += 8) {
            VkSpecializationInfo part = *spec;
            part.mapEntryCount = nspec - e < 8 ? nspec - e : 8;
            part.pMapEntries = spec->pMapEntries + e;
            spec_words(&part, w);
            for (uint32_t j = 0; j < 4 * part.mapEntryCount; j++)
                word(&b, w[j]);
        }
    }
    // What it leaves to commands of what WebGPU makes part of a pipeline:
    // 1 depth bias, 2 stencil compare mask, 4 stencil write mask.
    uint32_t dynamic = 0;
    if (info->pDynamicState)
        for (uint32_t k = 0; k < info->pDynamicState->dynamicStateCount; k++)
            switch (info->pDynamicState->pDynamicStates[k]) {
            case VK_DYNAMIC_STATE_DEPTH_BIAS: dynamic |= 1; break;
            case VK_DYNAMIC_STATE_STENCIL_COMPARE_MASK: dynamic |= 2; break;
            case VK_DYNAMIC_STATE_STENCIL_WRITE_MASK: dynamic |= 4; break;
            default: break;
            }
    word(&b, dynamic);
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
    p->written = q.r[2];
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
    const VkRenderPassMultiviewCreateInfo *multiview = NULL;
    for (const VkBaseInStructure *x = info->pNext; x; x = x->pNext)
        if (x->sType == VK_STRUCTURE_TYPE_RENDER_PASS_MULTIVIEW_CREATE_INFO)
            multiview = (const VkRenderPassMultiviewCreateInfo *)x;
    for (uint32_t s = 0; s < p->nsub; s++) {
        const VkSubpassDescription *sd = &info->pSubpasses[s];
        struct subpass *sub = &p->sub[s];
        sub->views = multiview && s < multiview->subpassCount ? multiview->pViewMasks[s] : 0;
        // (Each color attachment with the one it's resolved into, if any.)
        for (uint32_t k = 0; k < sd->colorAttachmentCount && sub->ncolors < 8; k++)
            if (sd->pColorAttachments[k].attachment != VK_ATTACHMENT_UNUSED) {
                sub->resolves[sub->ncolors] = sd->pResolveAttachments ? sd->pResolveAttachments[k].attachment : VK_ATTACHMENT_UNUSED;
                sub->colors[sub->ncolors++] = sd->pColorAttachments[k].attachment;
            }
        sub->depth = sd->pDepthStencilAttachment ? sd->pDepthStencilAttachment->attachment : VK_ATTACHMENT_UNUSED;
        for (uint32_t k = 0; k <= 2 * sub->ncolors; k++) {
            uint32_t at = k < sub->ncolors ? sub->colors[k] : k < 2 * sub->ncolors ? sub->resolves[k - sub->ncolors] : sub->depth;
            if (at == VK_ATTACHMENT_UNUSED)
                continue;
            if (p->first[at] == VK_ATTACHMENT_UNUSED)
                p->first[at] = s;
            p->last[at] = s;
        }
        // (An input attachment is a use too: read, so stored until then.)
        for (uint32_t k = 0; k < sd->inputAttachmentCount; k++) {
            uint32_t at = sd->pInputAttachments[k].attachment;
            if (at == VK_ATTACHMENT_UNUSED)
                continue;
            if (p->first[at] == VK_ATTACHMENT_UNUSED)
                p->first[at] = s;
            if (p->last[at] < s)
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
    f->width = info->width;
    f->height = info->height;
    f->views = views;
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

static int texel_descriptor(VkDescriptorType t) {
    return t == VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER || t == VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER;
}
static int buffer_descriptor(VkDescriptorType t) {
    return t == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER || t == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER || t == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC ||
           t == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC;
}

static int image_descriptor(VkDescriptorType t) {
    return t == VK_DESCRIPTOR_TYPE_SAMPLER || t == VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER || t == VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE ||
           t == VK_DESCRIPTOR_TYPE_STORAGE_IMAGE || t == VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT;
}

static CALL VkResult CC CreateDescriptorSetLayout(VkDevice d, const VkDescriptorSetLayoutCreateInfo *info, const VkAllocationCallbacks *a, VkDescriptorSetLayout *out) {
    for (uint32_t i = 0; i < info->bindingCount; i++) {
        const VkDescriptorSetLayoutBinding *b = &info->pBindings[i];
        if (b->descriptorCount && !buffer_descriptor(b->descriptorType) && !image_descriptor(b->descriptorType) && !texel_descriptor(b->descriptorType))
            return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    struct VkDescriptorSetLayout_T *l = zalloc(sizeof *l);
    if (!l || !(l->bindings = zalloc(info->bindingCount * sizeof *l->bindings)) || !(l->first = zalloc(info->bindingCount * sizeof *l->first))) {
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
    for (uint32_t k = 0; k < l->n; k++) {
        l->first[k] = l->total;
        l->total += l->bindings[k].descriptorCount;
    }
    if (!(l->immutable = zalloc(l->total * sizeof *l->immutable))) {
        free(l->bindings), free(l->first), free(l);
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    // (Immutable samplers kept: the program's array needn't outlive this.)
    for (uint32_t k = 0; k < l->n; k++) {
        VkDescriptorSetLayoutBinding *b = &l->bindings[k];
        if ((b->descriptorType == VK_DESCRIPTOR_TYPE_SAMPLER || b->descriptorType == VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER) && b->pImmutableSamplers)
            for (uint32_t e = 0; e < b->descriptorCount; e++)
                l->immutable[l->first[k] + e] = b->pImmutableSamplers[e];
        b->pImmutableSamplers = NULL;
    }
    *out = l;
    return VK_SUCCESS;
}
static CALL void CC DestroyDescriptorSetLayout(VkDevice d, VkDescriptorSetLayout l, const VkAllocationCallbacks *a) {
    if (l) {
        free(l->bindings);
        free(l->first);
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
        if (!s || !(s->d = zalloc(l->total * sizeof *s->d))) {
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
            for (uint32_t e = 0; e < l->bindings[k].descriptorCount; e++) {
                struct descriptor *x = &s->d[l->first[k] + e];
                x->type = l->bindings[k].descriptorType;
                x->binding = l->bindings[k].binding;
                x->element = e;
                x->sampler = l->immutable[l->first[k] + e];
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
static struct descriptor *slot(VkDescriptorSet s, uint32_t binding, uint32_t element) {
    VkDescriptorSetLayout l = s->layout;
    uint32_t k = 0;
    while (k < l->n && l->bindings[k].binding != binding)
        k++;
    // (Past a binding's last element, Vulkan goes on into the next's.)
    while (k < l->n && element >= l->bindings[k].descriptorCount) {
        element -= l->bindings[k].descriptorCount;
        k++;
    }
    return k < l->n ? &s->d[l->first[k] + element] : NULL;
}

static CALL void CC UpdateDescriptorSets(VkDevice d, uint32_t nw, const VkWriteDescriptorSet *w, uint32_t nc, const VkCopyDescriptorSet *c) {
    for (uint32_t i = 0; i < nw; i++) {
        VkDescriptorType t = w[i].descriptorType;
        for (uint32_t k = 0; k < w[i].descriptorCount; k++) {
            struct descriptor *e = slot(w[i].dstSet, w[i].dstBinding, w[i].dstArrayElement + k);
            if (!e)
                continue;
            if (buffer_descriptor(t)) {
                const VkDescriptorBufferInfo *b = &w[i].pBufferInfo[k];
                e->buffer = b->buffer;
                e->offset = b->offset;
                e->range = b->range == VK_WHOLE_SIZE ? b->buffer->size - b->offset : b->range;
            } else if (texel_descriptor(t)) {
                e->texel = w[i].pTexelBufferView[k];
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
            struct descriptor *from = slot(c[i].srcSet, c[i].srcBinding, c[i].srcArrayElement + k), *to = slot(c[i].dstSet, c[i].dstBinding, c[i].dstArrayElement + k);
            if (from && to) {
                uint32_t binding = to->binding, element = to->element;
                *to = *from;
                to->binding = binding, to->element = element;
            }
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
    c->n = c->nwrites = c->nreads = c->nsignals = 0;
    c->bound[0] = c->bound[1] = NULL;
    memset(c->nstores, 0, sizeof c->nstores);
    c->failed = 0;
    c->state.set = 0;
    c->pass = NULL;
}

static CALL void CC FreeCommandBuffers(VkDevice d, VkCommandPool p, uint32_t n, const VkCommandBuffer *cs) {
    for (uint32_t i = 0; i < n; i++)
        if (cs[i]) {
            free(cs[i]->s);
            free(cs[i]->writes);
            free(cs[i]->reads);
            for (int p = 0; p < 2; p++)
                for (int s = 0; s < 8; s++)
                    free(cs[i]->stores[p][s]);
            free(cs[i]->clears);
            free(cs[i]->signals);
            free(cs[i]);
        }
}

static CALL VkResult CC BeginCommandBuffer(VkCommandBuffer c, const VkCommandBufferBeginInfo *info) {
    reset(c);
    // A secondary command buffer that continues a render pass records as
    // one inside it (its commands go into the primary's pass).
    const VkCommandBufferInheritanceInfo *in = info->pInheritanceInfo;
    if ((info->flags & VK_COMMAND_BUFFER_USAGE_RENDER_PASS_CONTINUE_BIT) && in && in->renderPass) {
        c->pass = in->renderPass;
        c->framebuffer = in->framebuffer;
        c->subpass = in->subpass;
    }
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

// That a command may read a buffer's range (sent from mapped memory as the
// command buffer is submitted).
static void reads(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, VkDeviceSize size) {
    if (c->nreads == c->rcap) {
        size_t cap = c->rcap ? c->rcap * 2 : 16;
        struct write *w = realloc(c->reads, cap * sizeof *w);
        if (!w) {
            c->failed = 1;
            return;
        }
        c->reads = w;
        c->rcap = cap;
    }
    c->reads[c->nreads++] = (struct write){b->memory, b->offset + offset, b->offset + offset + size};
}

// Graphics state set (the bits `which`), into the render pass begun.
static void emit_state(VkCommandBuffer c, uint32_t which) {
    struct state *s = &c->state;
    which &= s->set;
    if (!c->pass)
        return;
    if (which & S_VIEWPORT)
        command(c, SET_VIEWPORT, s->viewport, sizeof s->viewport, NULL, 0);
    // (The runtime keeps scissors within the attachments.)
    if (which & S_SCISSOR)
        command(c, SET_SCISSOR, s->scissor, sizeof s->scissor, NULL, 0);
    if (which & S_BLEND)
        command(c, SET_BLEND_CONSTANTS, s->blend, sizeof s->blend, NULL, 0);
    if (which & S_REFERENCE)
        command(c, SET_STENCIL_REFERENCE, &s->reference, 4, NULL, 0);
    if (which & S_BIAS)
        command(c, SET_DEPTH_BIAS, s->bias, sizeof s->bias, NULL, 0);
    if (which & S_COMPARE_MASK)
        command(c, SET_STENCIL_COMPARE_MASK, &s->compare_mask, 4, NULL, 0);
    if (which & S_WRITE_MASK)
        command(c, SET_STENCIL_WRITE_MASK, &s->write_mask, 4, NULL, 0);
}

static CALL void CC CmdBindPipeline(VkCommandBuffer c, VkPipelineBindPoint point, VkPipeline p) {
    command(c, BIND_PIPELINE, &p->handle, 8, NULL, 0);
    c->bound[point == VK_PIPELINE_BIND_POINT_COMPUTE] = p;
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

// A storage descriptor bound (bind point p, set), for `wrote`.
static void store(VkCommandBuffer c, uint32_t p, uint32_t set, uint32_t binding, VkBuffer b, VkDeviceSize offset, VkDeviceSize range) {
    if (set >= 8)
        return;
    if (c->nstores[p][set] == c->scap[p][set]) {
        uint32_t cap = c->scap[p][set] ? 2 * c->scap[p][set] : 8;
        struct stored *s = realloc(c->stores[p][set], cap * sizeof *s);
        if (!s) {
            c->failed = 1;
            return;
        }
        c->stores[p][set] = s;
        c->scap[p][set] = cap;
    }
    c->stores[p][set][c->nstores[p][set]++] = (struct stored){binding, b, offset, range};
}

// A draw or dispatch (bind point p): what its pipeline writes of the
// storage descriptors bound, read back after a wait.
static void wrote(VkCommandBuffer c, uint32_t p) {
    VkPipeline pl = c->bound[p];
    if (!pl || !pl->written)
        return;
    for (uint32_t set = 0; set < 8; set++)
        for (uint32_t k = 0; k < c->nstores[p][set]; k++) {
            struct stored *s = &c->stores[p][set][k];
            uint32_t bit = set * 16 + s->binding;
            if (pl->written == UINT64_MAX || (bit < 64 && (pl->written >> bit & 1)))
                writes(c, s->buffer, s->offset, s->range);
        }
}

static CALL void CC CmdBindDescriptorSets(VkCommandBuffer c, VkPipelineBindPoint point, VkPipelineLayout layout, uint32_t first, uint32_t n, const VkDescriptorSet *sets, uint32_t ndyn, const uint32_t *dyn) {
    uint32_t used = 0;
    for (uint32_t i = 0; i < n; i++) {
        VkDescriptorSet s = sets[i];
        if (!s)
            continue;
        // Its descriptors that are set (kind 0 a buffer, 1 a view, 2 a
        // sampler, 3 a view and sampler, 4 a texel buffer view:
        // gpu/src/vulkan.rs, `decode`).
        uint32_t count = 0;
        for (uint32_t k = 0; k < s->layout->total; k++)
            count += s->d[k].buffer || s->d[k].view || s->d[k].sampler || s->d[k].texel;
        uint32_t head[2] = {first + i, count};
        uint32_t h[2] = {point == VK_PIPELINE_BIND_POINT_GRAPHICS ? BIND_GRAPHICS_SET : BIND_SET, 8 + 8 + count * 32};
        // (Its storage descriptors, kept for the draws and dispatches.)
        uint32_t p = point == VK_PIPELINE_BIND_POINT_COMPUTE, set = first + i;
        if (set < 8)
            c->nstores[p][set] = 0;
        put(c, h, 8);
        put(c, head, 8);
        for (uint32_t k = 0; k < s->layout->total; k++) {
            struct descriptor *e = &s->d[k];
            VkDeviceSize offset = e->offset;
            if (e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC || e->type == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC)
                offset += used < ndyn ? dyn[used++] : 0;
            uint32_t b[2] = {e->binding, 0};
            uint64_t r[3] = {0, 0, 0};
            if (e->buffer) {
                r[0] = e->buffer->memory->handle;
                r[1] = e->buffer->offset + offset;
                r[2] = e->range;
                reads(c, e->buffer, offset, e->range);
                if (e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER || e->type == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC)
                    store(c, p, set, e->binding, e->buffer, offset, e->range);
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
            } else if (e->texel) {
                b[1] = 4;
                r[0] = e->texel->handle;
                reads(c, e->texel->buffer, e->texel->offset, e->texel->range);
                if (e->type == VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER)
                    store(c, p, set, e->binding, e->texel->buffer, e->texel->offset, e->texel->range);
            } else
                continue;
            // (An array's element above the kind's byte.)
            b[1] |= e->element << 8;
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
    wrote(c, 1);
    uint32_t g[3] = {x, y, z};
    command(c, DISPATCH, g, 12, NULL, 0);
}

static CALL void CC CmdDispatchIndirect(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset) {
    wrote(c, 1);
    uint64_t o[2] = {b->memory->handle, b->offset + offset};
    reads(c, b, offset, 12);
    command(c, DISPATCH_INDIRECT, o, 16, NULL, 0);
}

static CALL void CC CmdCopyBuffer(VkCommandBuffer c, VkBuffer src, VkBuffer dst, uint32_t n, const VkBufferCopy *r) {
    for (uint32_t i = 0; i < n; i++) {
        uint64_t o[5] = {src->memory->handle, src->offset + r[i].srcOffset, dst->memory->handle, dst->offset + r[i].dstOffset, r[i].size};
        reads(c, src, r[i].srcOffset, r[i].size);
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
        {
            // (The bytes the copy reaches in the buffer: written, or read.)
            // (A texel of the aspect copied: stencil's a byte; a depth and
            // stencil format's own size is none, so at most 4.)
            VkExtent3D e = r[i].imageExtent;
            VkDeviceSize texel = r[i].imageSubresource.aspectMask == VK_IMAGE_ASPECT_STENCIL_BIT ? 1 : im->texel ? im->texel : 4;
            VkDeviceSize row = (VkDeviceSize)(r[i].bufferRowLength ? r[i].bufferRowLength : e.width) * texel;
            VkDeviceSize height = r[i].bufferImageHeight ? r[i].bufferImageHeight : e.height;
            VkDeviceSize slices = im->type == VK_IMAGE_TYPE_3D ? e.depth : r[i].imageSubresource.layerCount;
            VkDeviceSize size = (slices - 1) * height * row + (e.height - 1) * row + (VkDeviceSize)e.width * texel;
            if (op == IMAGE_TO_BUFFER)
                writes(c, b, r[i].bufferOffset, size);
            else
                reads(c, b, r[i].bufferOffset, size);
        }
    }
}

// A copy between a buffer (rows of `row` texels, `height` rows an image)
// and a linear image's memory, a row at a time.
static void linear_rows(VkCommandBuffer c, VkBuffer b, VkDeviceSize at, uint32_t row, uint32_t height, VkImage im, VkOffset3D o, VkExtent3D e, int to_image) {
    for (uint32_t z = 0; z < e.depth; z++)
        for (uint32_t y = 0; y < e.height; y++) {
            VkDeviceSize in_buffer = at + ((VkDeviceSize)z * height + y) * row * im->texel;
            VkDeviceSize in_image = ((((VkDeviceSize)o.z + z) * im->extent.height + o.y + y) * im->extent.width + o.x) * im->texel;
            VkBufferCopy r = {to_image ? in_buffer : in_image, to_image ? in_image : in_buffer, (VkDeviceSize)e.width * im->texel};
            CmdCopyBuffer(c, to_image ? b : &im->memory, to_image ? &im->memory : b, 1, &r);
        }
}

static void linear_copies(VkCommandBuffer c, VkBuffer b, VkImage im, uint32_t n, const VkBufferImageCopy *r, int to_image) {
    for (uint32_t i = 0; i < n; i++) {
        VkExtent3D e = r[i].imageExtent;
        linear_rows(c, b, r[i].bufferOffset, r[i].bufferRowLength ? r[i].bufferRowLength : e.width, r[i].bufferImageHeight ? r[i].bufferImageHeight : e.height, im, r[i].imageOffset, e, to_image);
    }
}

static CALL void CC CmdCopyBufferToImage(VkCommandBuffer c, VkBuffer b, VkImage im, VkImageLayout l, uint32_t n, const VkBufferImageCopy *r) {
    if (im->linear)
        linear_copies(c, b, im, n, r, 1);
    else
        buffer_image(c, BUFFER_TO_IMAGE, b, im, n, r);
}

static CALL void CC CmdCopyImageToBuffer(VkCommandBuffer c, VkImage im, VkImageLayout l, VkBuffer b, uint32_t n, const VkBufferImageCopy *r) {
    if (im->linear)
        linear_copies(c, b, im, n, r, 0);
    else
        buffer_image(c, IMAGE_TO_BUFFER, b, im, n, r);
}

// Each region's layers (2D images), or its slices (3D: their z ranges),
// scaled.
static CALL void CC CmdBlitImage(VkCommandBuffer c, VkImage src, VkImageLayout sl, VkImage dst, VkImageLayout dl, uint32_t n, const VkImageBlit *r, VkFilter filter) {
    uint64_t h[2] = {src->handle, dst->handle};
    for (uint32_t i = 0; i < n; i++)
        for (uint32_t l = 0; l < r[i].srcSubresource.layerCount; l++) {
            const VkOffset3D *s = r[i].srcOffsets, *d = r[i].dstOffsets;
            uint32_t w[17] = {r[i].srcSubresource.mipLevel, r[i].srcSubresource.baseArrayLayer + l, (uint32_t)s[0].x, (uint32_t)s[0].y, (uint32_t)s[1].x, (uint32_t)s[1].y,
                              r[i].dstSubresource.mipLevel, r[i].dstSubresource.baseArrayLayer + l, (uint32_t)d[0].x, (uint32_t)d[0].y, (uint32_t)d[1].x, (uint32_t)d[1].y,
                              (uint32_t)filter, (uint32_t)s[0].z, (uint32_t)s[1].z, (uint32_t)d[0].z, (uint32_t)d[1].z};
            command(c, BLIT, h, sizeof h, w, sizeof w);
        }
}

// (A linear image's side of a copy is a buffer's: its memory.)
static CALL void CC CmdCopyImage(VkCommandBuffer c, VkImage src, VkImageLayout sl, VkImage dst, VkImageLayout dl, uint32_t n, const VkImageCopy *r) {
    for (uint32_t i = 0; i < n; i++) {
        if (src->linear || dst->linear) {
            VkImage linear = src->linear ? src : dst, other = src->linear ? dst : src;
            VkOffset3D lo = src->linear ? r[i].srcOffset : r[i].dstOffset;
            VkDeviceSize at = (((VkDeviceSize)lo.z * linear->extent.height + lo.y) * linear->extent.width + lo.x) * linear->texel;
            if (other->linear) {
                // (Both: the source's rows into the destination's.)
                linear_rows(c, &src->memory, at, src->extent.width, src->extent.height, dst, r[i].dstOffset, r[i].extent, 1);
                continue;
            }
            VkBufferImageCopy b = {at, linear->extent.width, linear->extent.height, src->linear ? r[i].dstSubresource : r[i].srcSubresource,
                                   src->linear ? r[i].dstOffset : r[i].srcOffset, r[i].extent};
            buffer_image(c, src->linear ? BUFFER_TO_IMAGE : IMAGE_TO_BUFFER, &linear->memory, other, 1, &b);
            continue;
        }
        uint32_t h[2] = {COPY_IMAGE, 8 + 8 + 40 + 8 + 40};
        put(c, h, 8);
        put(c, &src->handle, 8);
        sub(c, &r[i].srcSubresource, r[i].srcOffset, r[i].extent);
        put(c, &dst->handle, 8);
        sub(c, &r[i].dstSubresource, r[i].dstOffset, r[i].extent);
    }
}

static CALL void CC CmdClearColorImage(VkCommandBuffer c, VkImage im, VkImageLayout l, const VkClearColorValue *color, uint32_t n, const VkImageSubresourceRange *r) {
    // (A linear image: its memory filled with the color, packed.)
    if (im->linear) {
        uint64_t o[3] = {im->memory.memory->handle, im->memory.offset, im->memory.size};
        uint32_t w[5] = {(uint32_t)im->format, color->uint32[0], color->uint32[1], color->uint32[2], color->uint32[3]};
        command(c, FILL_TEXELS, o, sizeof o, w, sizeof w);
        writes(c, &im->memory, 0, im->memory.size);
        return;
    }
    for (uint32_t i = 0; i < n; i++) {
        uint32_t mips = r[i].levelCount == VK_REMAINING_MIP_LEVELS ? im->mips - r[i].baseMipLevel : r[i].levelCount;
        uint32_t layers = r[i].layerCount == VK_REMAINING_ARRAY_LAYERS ? im->layers - r[i].baseArrayLayer : r[i].layerCount;
        uint32_t w[8] = {r[i].baseMipLevel, mips, r[i].baseArrayLayer, layers, color->uint32[0], color->uint32[1], color->uint32[2], color->uint32[3]};
        command(c, CLEAR_COLOR, &im->handle, 8, w, sizeof w);
    }
}

static CALL void CC CmdClearDepthStencilImage(VkCommandBuffer c, VkImage im, VkImageLayout l, const VkClearDepthStencilValue *v, uint32_t n, const VkImageSubresourceRange *r) {
    for (uint32_t i = 0; i < n; i++) {
        uint32_t mips = r[i].levelCount == VK_REMAINING_MIP_LEVELS ? im->mips - r[i].baseMipLevel : r[i].levelCount;
        uint32_t layers = r[i].layerCount == VK_REMAINING_ARRAY_LAYERS ? im->layers - r[i].baseArrayLayer : r[i].layerCount;
        uint32_t w[7] = {r[i].baseMipLevel, mips, r[i].baseArrayLayer, layers, r[i].aspectMask, bits(v->depth), v->stencil};
        command(c, CLEAR_DEPTH, &im->handle, 8, w, sizeof w);
    }
}

// (Whole levels and layers: WebGPU resolves whole attachments.)
static CALL void CC CmdResolveImage(VkCommandBuffer c, VkImage src, VkImageLayout sl, VkImage dst, VkImageLayout dl, uint32_t n, const VkImageResolve *r) {
    for (uint32_t i = 0; i < n; i++) {
        uint64_t images[2] = {src->handle, dst->handle};
        uint32_t w[5] = {r[i].srcSubresource.mipLevel, r[i].srcSubresource.baseArrayLayer, r[i].dstSubresource.mipLevel, r[i].dstSubresource.baseArrayLayer, r[i].srcSubresource.layerCount};
        command(c, RESOLVE, images, sizeof images, w, sizeof w);
    }
}

// Inside a render pass: the attachments named (a color attachment of the
// subpass, or its depth and stencil) cleared within the rectangles.
static CALL void CC CmdClearAttachments(VkCommandBuffer c, uint32_t n, const VkClearAttachment *a, uint32_t nrects, const VkClearRect *rects) {
    uint32_t head[2] = {n, nrects};
    uint32_t h[2] = {CLEAR_ATTACHMENTS, 8 + 8 + n * 24 + nrects * 24};
    put(c, h, 8);
    put(c, head, 8);
    for (uint32_t i = 0; i < n; i++) {
        uint32_t w[6] = {a[i].aspectMask, a[i].colorAttachment};
        if (a[i].aspectMask & VK_IMAGE_ASPECT_COLOR_BIT)
            memcpy(w + 2, a[i].clearValue.color.uint32, 16);
        else
            w[2] = bits(a[i].clearValue.depthStencil.depth), w[3] = a[i].clearValue.depthStencil.stencil;
        put(c, w, sizeof w);
    }
    for (uint32_t i = 0; i < nrects; i++) {
        const VkClearRect *r = &rects[i];
        uint32_t w[6] = {(uint32_t)r->rect.offset.x, (uint32_t)r->rect.offset.y, r->rect.extent.width, r->rect.extent.height, r->baseArrayLayer, r->layerCount};
        put(c, w, sizeof w);
    }
    // (The clear draws: the pass's state set again after.)
    emit_state(c, ~0u);
}

// ---- Queries: occlusion (inside render passes) and timestamps ----

static CALL VkResult CC CreateQueryPool(VkDevice d, const VkQueryPoolCreateInfo *info, const VkAllocationCallbacks *a, VkQueryPool *out) {
    struct VkQueryPool_T *p = zalloc(sizeof *p);
    if (!p)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    struct request q = {VK_QUERY_POOL, {(uint64_t)info->queryType, info->queryCount}};
    if (call(&q)) {
        free(p);
        return VK_ERROR_FEATURE_NOT_PRESENT;
    }
    p->handle = q.r[0];
    p->count = info->queryCount;
    *out = p;
    return VK_SUCCESS;
}

static CALL void CC DestroyQueryPool(VkDevice d, VkQueryPool p, const VkAllocationCallbacks *a) {
    if (!p)
        return;
    struct request q = {FREE, {p->handle}};
    call(&q);
    free(p);
}

// Results as of the last submission that ended them (submissions are
// done when they return): those not ended since their reset aren't
// available.
static CALL VkResult CC GetQueryPoolResults(VkDevice d, VkQueryPool p, uint32_t first, uint32_t n, size_t size, void *data, VkDeviceSize stride, VkQueryResultFlags flags) {
    if (first + n > p->count)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    uint64_t *values = malloc((n ? n : 1) * sizeof *values);
    if (!values)
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    struct request q = {VK_QUERY_RESULTS, {p->handle, first, n, (uint64_t)(uintptr_t)values}};
    if (call(&q)) {
        free(values);
        return VK_ERROR_DEVICE_LOST;
    }
    VkResult result = VK_SUCCESS;
    int wide = !!(flags & VK_QUERY_RESULT_64_BIT);
    for (uint32_t k = 0; k < n; k++) {
        uint8_t *at = (uint8_t *)data + k * stride;
        int available = values[k] != UINT64_MAX;
        if (!available)
            result = VK_NOT_READY;
        if (available || (flags & VK_QUERY_RESULT_PARTIAL_BIT)) {
            uint64_t v = available ? values[k] : 0;
            memcpy(at, &v, wide ? 8 : 4);
        }
        if (flags & VK_QUERY_RESULT_WITH_AVAILABILITY_BIT) {
            uint64_t a = available;
            memcpy(at + (wide ? 8 : 4), &a, wide ? 8 : 4);
        }
    }
    free(values);
    (void)size;
    return result;
}

static CALL void CC CmdBeginQuery(VkCommandBuffer c, VkQueryPool p, uint32_t index, VkQueryControlFlags flags) {
    uint32_t w[2] = {index, flags};
    command(c, BEGIN_QUERY, &p->handle, 8, w, sizeof w);
}

static CALL void CC CmdEndQuery(VkCommandBuffer c, VkQueryPool p, uint32_t index) {
    command(c, END_QUERY, &p->handle, 8, &index, 4);
}

static CALL void CC CmdResetQueryPool(VkCommandBuffer c, VkQueryPool p, uint32_t first, uint32_t n) {
    uint32_t w[2] = {first, n};
    command(c, RESET_QUERIES, &p->handle, 8, w, sizeof w);
}

static CALL void CC CmdWriteTimestamp(VkCommandBuffer c, VkPipelineStageFlagBits stage, VkQueryPool p, uint32_t index) {
    command(c, TIMESTAMP, &p->handle, 8, &index, 4);
}

static CALL void CC CmdCopyQueryPoolResults(VkCommandBuffer c, VkQueryPool p, uint32_t first, uint32_t n, VkBuffer b, VkDeviceSize offset, VkDeviceSize stride, VkQueryResultFlags flags) {
    uint32_t h[2] = {COPY_QUERIES, 8 + 8 + 8 + 24 + 4};
    uint32_t w[2] = {first, n};
    uint64_t at[3] = {b->memory->handle, b->offset + offset, stride};
    uint32_t f = flags;
    put(c, h, 8);
    put(c, &p->handle, 8);
    put(c, w, 8);
    put(c, at, sizeof at);
    put(c, &f, 4);
    if (n)
        writes(c, b, offset, (n - 1) * stride + ((flags & VK_QUERY_RESULT_64_BIT) ? 16 : 8));
}

// ---- Drawing ----

// A subpass begun: a WebGPU render pass of its attachments, which load as
// the render pass says where it first uses them (else what's there), and
// store unless where it last does the render pass says not to.
// The views of subpass `s` that use attachment `at` first (multiview: a
// load op is each view's, where it first uses it).
static uint32_t first_views(VkRenderPass p, uint32_t s, uint32_t at) {
    uint32_t before = 0;
    for (uint32_t t = 0; t < s; t++) {
        const struct subpass *u = &p->sub[t];
        int uses = u->depth == at;
        for (uint32_t k = 0; k < u->ncolors; k++)
            uses |= u->colors[k] == at || u->resolves[k] == at;
        if (uses)
            before |= u->views;
    }
    return p->sub[s].views & ~before;
}

static void begin_subpass(VkCommandBuffer c) {
    VkRenderPass p = c->pass;
    VkFramebuffer f = c->framebuffer;
    uint32_t s = c->subpass;
    const struct subpass *sp = &p->sub[s];
    uint32_t head[2] = {sp->ncolors, sp->depth != VK_ATTACHMENT_UNUSED};
    uint32_t h[2] = {BEGIN_PASS, 8 + 8 + sp->ncolors * 44 + (head[1] ? 36 : 0) + 4 + 8};
    put(c, h, 8);
    put(c, head, 8);
    VkClearValue zero = {0};
    // (A render area smaller than the framebuffer: what clears as the pass
    // begins loads instead, and is cleared within the area after.)
    int partial = c->area.offset.x > 0 || c->area.offset.y > 0 || c->area.extent.width < f->width || c->area.extent.height < f->height;
    VkClearAttachment within[9];
    uint32_t nwithin = 0;
    for (uint32_t k = 0; k <= sp->ncolors; k++) {
        uint32_t at = k < sp->ncolors ? sp->colors[k] : sp->depth;
        if (at == VK_ATTACHMENT_UNUSED)
            continue;
        const VkAttachmentDescription *d = &p->attachments[at];
        const VkClearValue *clear = at < c->nclears ? &c->clears[at] : &zero;
        uint32_t load = p->first[at] == s ? (uint32_t)d->loadOp : 0, store = p->last[at] == s ? (uint32_t)d->storeOp : 0;
        uint32_t firsts = sp->views ? first_views(p, s, at) : 0;
        if (sp->views && firsts)
            load = (uint32_t)d->loadOp;
        uint32_t w[6];
        if (partial && k < sp->ncolors && load == VK_ATTACHMENT_LOAD_OP_CLEAR) {
            within[nwithin++] = (VkClearAttachment){VK_IMAGE_ASPECT_COLOR_BIT, k, *clear};
            load = VK_ATTACHMENT_LOAD_OP_LOAD;
        }
        if (k < sp->ncolors) {
            w[0] = load, w[1] = store;
            memcpy(w + 2, clear->color.uint32, 16);
        } else {
            uint32_t sload = p->first[at] == s || (sp->views && firsts) ? (uint32_t)d->stencilLoadOp : 0, sstore = p->last[at] == s ? (uint32_t)d->stencilStoreOp : 0;
            VkImageAspectFlags aspects = 0;
            if (partial && load == VK_ATTACHMENT_LOAD_OP_CLEAR)
                aspects |= VK_IMAGE_ASPECT_DEPTH_BIT, load = VK_ATTACHMENT_LOAD_OP_LOAD;
            if (partial && sload == VK_ATTACHMENT_LOAD_OP_CLEAR)
                aspects |= VK_IMAGE_ASPECT_STENCIL_BIT, sload = VK_ATTACHMENT_LOAD_OP_LOAD;
            if (aspects)
                within[nwithin++] = (VkClearAttachment){aspects, 0, *clear};
            w[0] = load, w[1] = store, w[2] = sload, w[3] = sstore;
            w[4] = bits(clear->depthStencil.depth), w[5] = clear->depthStencil.stencil;
        }
        put(c, &f->views[at]->handle, 8);
        put(c, w, sizeof w);
        // (A color attachment's resolve attachment's view, or none.)
        if (k < sp->ncolors) {
            uint64_t none = 0;
            uint32_t r = sp->resolves[k];
            put(c, r != VK_ATTACHMENT_UNUSED ? &f->views[r]->handle : &none, 8);
        }
        // (The views its load ops are for.)
        put(c, &firsts, 4);
    }
    put(c, &sp->views, 4);
    // (The framebuffer's size: a pass of no attachments has its own.)
    uint32_t frame[2] = {f->width, f->height};
    put(c, frame, 8);
    if (nwithin) {
        VkClearRect area = {c->area, 0, 1};
        CmdClearAttachments(c, nwithin, within, 1, &area);
        return;
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
    c->area = info->renderArea;
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
// (Kept, and sent into the pass: the draws' pipelines are made with them,
// where they leave them to commands.)
static CALL void CC CmdSetDepthBias(VkCommandBuffer c, float constant, float clamp, float slope) {
    struct state *s = &c->state;
    s->bias[0] = constant, s->bias[1] = clamp, s->bias[2] = slope;
    s->set |= S_BIAS;
    emit_state(c, S_BIAS);
}
static CALL void CC CmdSetDepthBounds(VkCommandBuffer c, float lo, float hi) {}
// (WebGPU has one mask for both faces: the last set.)
static CALL void CC CmdSetStencilCompareMask(VkCommandBuffer c, VkStencilFaceFlags faces, uint32_t mask) {
    c->state.compare_mask = mask;
    c->state.set |= S_COMPARE_MASK;
    emit_state(c, S_COMPARE_MASK);
}
static CALL void CC CmdSetStencilWriteMask(VkCommandBuffer c, VkStencilFaceFlags faces, uint32_t mask) {
    c->state.write_mask = mask;
    c->state.set |= S_WRITE_MASK;
    emit_state(c, S_WRITE_MASK);
}

static CALL void CC CmdBindVertexBuffers(VkCommandBuffer c, uint32_t first, uint32_t n, const VkBuffer *b, const VkDeviceSize *offsets) {
    uint32_t head[2] = {first, n};
    uint32_t h[2] = {BIND_VERTEX, 8 + 8 + n * 16};
    put(c, h, 8);
    put(c, head, 8);
    for (uint32_t k = 0; k < n; k++) {
        uint64_t r[2] = {b[k]->memory->handle, b[k]->offset + offsets[k]};
        put(c, r, 16);
        reads(c, b[k], offsets[k], b[k]->size - offsets[k]);
    }
}

static CALL void CC CmdBindIndexBuffer(VkCommandBuffer c, VkBuffer b, VkDeviceSize offset, VkIndexType type) {
    reads(c, b, offset, b->size - offset);
    uint64_t r[2] = {b->memory->handle, b->offset + offset};
    uint32_t t = (uint32_t)type;
    command(c, BIND_INDEX, r, 16, &t, 4);
}

static CALL void CC CmdDraw(VkCommandBuffer c, uint32_t vertices, uint32_t instances, uint32_t first, uint32_t first_instance) {
    wrote(c, 0);
    uint32_t w[4] = {vertices, instances, first, first_instance};
    command(c, DRAW, w, sizeof w, NULL, 0);
}

static CALL void CC CmdDrawIndexed(VkCommandBuffer c, uint32_t indices, uint32_t instances, uint32_t first, int32_t vertex_offset, uint32_t first_instance) {
    wrote(c, 0);
    uint32_t w[5] = {indices, instances, first, (uint32_t)vertex_offset, first_instance};
    command(c, DRAW_INDEXED, w, sizeof w, NULL, 0);
}

static void draw_indirect(VkCommandBuffer c, uint32_t op, VkBuffer b, VkDeviceSize offset, uint32_t count, uint32_t stride) {
    wrote(c, 0);
    reads(c, b, offset, count ? (VkDeviceSize)(count - 1) * stride + 20 : 0);
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
// (Barriers are the replay's order, but one inside a render pass: a
// subpass reading what it drew, which the runtime's pass ends for.)
static CALL void CC CmdPipelineBarrier(VkCommandBuffer c, VkPipelineStageFlags src, VkPipelineStageFlags dst, VkDependencyFlags f, uint32_t nm, const VkMemoryBarrier *m, uint32_t nb, const VkBufferMemoryBarrier *b, uint32_t ni, const VkImageMemoryBarrier *im) {
    if (c->pass)
        command(c, BARRIER, NULL, 0, NULL, 0);
}
// (Events set and reset by a command buffer: as it's submitted.)
static void event_op(VkCommandBuffer c, VkEvent e, int set) {
    if (c->nsignals == c->sigcap) {
        size_t cap = c->sigcap ? c->sigcap * 2 : 8;
        struct signal *g = realloc(c->signals, cap * sizeof *g);
        if (!g) {
            c->failed = 1;
            return;
        }
        c->signals = g;
        c->sigcap = cap;
    }
    c->signals[c->nsignals++] = (struct signal){e, set};
}
static CALL void CC CmdSetEvent(VkCommandBuffer c, VkEvent e, VkPipelineStageFlags s) {
    event_op(c, e, 1);
}
static CALL void CC CmdResetEvent(VkCommandBuffer c, VkEvent e, VkPipelineStageFlags s) {
    event_op(c, e, 0);
}
static CALL void CC CmdWaitEvents(VkCommandBuffer c, uint32_t n, const VkEvent *e, VkPipelineStageFlags src, VkPipelineStageFlags dst, uint32_t nm, const VkMemoryBarrier *m, uint32_t nb, const VkBufferMemoryBarrier *b, uint32_t ni, const VkImageMemoryBarrier *im) {}

static CALL void CC CmdExecuteCommands(VkCommandBuffer c, uint32_t n, const VkCommandBuffer *cs) {
    for (uint32_t i = 0; i < n; i++) {
        put(c, cs[i]->s, cs[i]->n);
        for (size_t k = 0; k < cs[i]->nsignals; k++)
            event_op(c, cs[i]->signals[k].event, cs[i]->signals[k].set);
        for (size_t k = 0; k < cs[i]->nwrites; k++) {
            struct write w = cs[i]->writes[k];
            struct VkBuffer_T b = {w.end - w.offset, 0, w.memory, w.offset};
            writes(c, &b, 0, w.end - w.offset);
        }
        for (size_t k = 0; k < cs[i]->nreads; k++) {
            struct write w = cs[i]->reads[k];
            struct VkBuffer_T b = {w.end - w.offset, 0, w.memory, w.offset};
            reads(c, &b, 0, w.end - w.offset);
        }
    }
}

// ---- Submission and synchronization ----

// Before a submission: the device's writes to mapped memory read back,
// then what the submission's command buffers may read of the program's
// copies sent (the runtime sends what changed: VK_SEND). The rest waits for
// a submission that reads it.
static int by_range(const void *a, const void *b) {
    const struct write *x = a, *y = b;
    if (x->memory != y->memory)
        return (uintptr_t)x->memory < (uintptr_t)y->memory ? -1 : 1;
    return x->offset < y->offset ? -1 : x->offset > y->offset;
}

static int send_mapped(VkDevice d, uint32_t n, const VkSubmitInfo *info) {
    for (VkDeviceMemory m = d->mapped; m; m = m->next)
        if (catch_up(m))
            return -1;
    size_t total = 0;
    for (uint32_t i = 0; i < n; i++)
        for (uint32_t k = 0; k < info[i].commandBufferCount; k++)
            total += info[i].pCommandBuffers[k]->nreads;
    struct write *all = malloc((total ? total : 1) * sizeof *all);
    if (!all)
        return -1;
    size_t used = 0;
    for (uint32_t i = 0; i < n; i++)
        for (uint32_t k = 0; k < info[i].commandBufferCount; k++) {
            VkCommandBuffer c = info[i].pCommandBuffers[k];
            for (size_t j = 0; j < c->nreads; j++)
                if (c->reads[j].memory->map)
                    all[used++] = c->reads[j];
        }
    // (Each memory's ranges merged, within it.)
    qsort(all, used, sizeof *all, by_range);
    int failed = 0;
    for (size_t j = 0; j < used;) {
        struct write r = all[j++];
        while (j < used && all[j].memory == r.memory && all[j].offset <= r.end) {
            if (all[j].end > r.end)
                r.end = all[j].end;
            j++;
        }
        if (r.end > r.memory->size)
            r.end = r.memory->size;
        failed |= transfer(VK_SEND, r.memory, r.offset, r.end);
    }
    free(all);
    return failed ? -1 : 0;
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
    if (send_mapped(d, n, info))
        return VK_ERROR_DEVICE_LOST;
    for (uint32_t i = 0; i < n; i++)
        for (uint32_t k = 0; k < info[i].commandBufferCount; k++) {
            VkCommandBuffer c = info[i].pCommandBuffers[k];
            for (size_t g = 0; g < c->nsignals; g++)
                c->signals[g].event->set = c->signals[g].set;
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

static CALL VkBool32 CC GetPhysicalDeviceWaylandPresentationSupportKHR(VkPhysicalDevice pd, uint32_t family, struct wl_display *display) {
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

// wl_shm's formats, which are BGRA in memory; and RGBA, swapped as
// presented.
static const VkSurfaceFormatKHR surface_formats[] = {
    {VK_FORMAT_B8G8R8A8_SRGB, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR},
    {VK_FORMAT_B8G8R8A8_UNORM, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR},
    {VK_FORMAT_R8G8B8A8_SRGB, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR},
    {VK_FORMAT_R8G8B8A8_UNORM, VK_COLOR_SPACE_SRGB_NONLINEAR_KHR},
};

static CALL VkResult CC GetPhysicalDeviceSurfaceFormatsKHR(VkPhysicalDevice pd, VkSurfaceKHR surface, uint32_t *count, VkSurfaceFormatKHR *out) {
    return enumerate(count, out, surface_formats, sizeof surface_formats / sizeof *surface_formats, sizeof *out);
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
    // For an RGBA swapchain: a BGRA image each presented one is drawn
    // into first (wl_shm's formats are BGRA).
    VkImage bgra;
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
    if (s->bgra)
        DestroyImage(d, s->bgra, NULL);
    free(s->slots);
    free(s);
}

static CALL VkResult CC CreateSwapchainKHR(VkDevice d, const VkSwapchainCreateInfoKHR *info, const VkAllocationCallbacks *a, VkSwapchainKHR *out) {
    // (The loader makes the surface: VkIcdSurfaceWayland.)
    VkIcdSurfaceWayland *surface = (VkIcdSurfaceWayland *)info->surface;
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
    // (RGBA: drawn into BGRA as presented, sRGB kept sRGB.)
    int srgb = info->imageFormat == VK_FORMAT_R8G8B8A8_SRGB;
    if (srgb || info->imageFormat == VK_FORMAT_R8G8B8A8_UNORM) {
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                                .imageType = VK_IMAGE_TYPE_2D,
                                .format = srgb ? VK_FORMAT_B8G8R8A8_SRGB : VK_FORMAT_B8G8R8A8_UNORM,
                                .extent = {s->width, s->height, 1},
                                .mipLevels = 1,
                                .arrayLayers = 1,
                                .samples = VK_SAMPLE_COUNT_1_BIT,
                                .tiling = VK_IMAGE_TILING_OPTIMAL,
                                .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT};
        if ((r = CreateImage(d, &ii, NULL, &s->bgra)) != VK_SUCCESS)
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

// Device groups' presenting (core 1.1 with VK_KHR_swapchain): the one
// device's, presenting locally.
static CALL VkResult CC AcquireNextImage2KHR(VkDevice d, const VkAcquireNextImageInfoKHR *i, uint32_t *index) {
    return AcquireNextImageKHR(d, i->swapchain, i->timeout, i->semaphore, i->fence, index);
}

static CALL VkResult CC GetDeviceGroupPresentCapabilitiesKHR(VkDevice d, VkDeviceGroupPresentCapabilitiesKHR *c) {
    memset(c->presentMask, 0, sizeof c->presentMask);
    c->presentMask[0] = 1;
    c->modes = VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR;
    return VK_SUCCESS;
}

static CALL VkResult CC GetDeviceGroupSurfacePresentModesKHR(VkDevice d, VkSurfaceKHR surface, VkDeviceGroupPresentModeFlagsKHR *modes) {
    *modes = VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR;
    return VK_SUCCESS;
}

// (The surface whole.)
static CALL VkResult CC GetPhysicalDevicePresentRectanglesKHR(VkPhysicalDevice pd, VkSurfaceKHR surface, uint32_t *count, VkRect2D *rects) {
    VkSurfaceCapabilitiesKHR c;
    GetPhysicalDeviceSurfaceCapabilitiesKHR(pd, surface, &c);
    VkRect2D r = {{0, 0}, c.currentExtent};
    return enumerate(count, rects, &r, 1, sizeof r);
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
    VkImage shown = sl->image;
    if (s->bgra) {
        VkImageBlit all = {{VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {{0, 0, 0}, {(int32_t)s->width, (int32_t)s->height, 1}},
                           {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {{0, 0, 0}, {(int32_t)s->width, (int32_t)s->height, 1}}};
        CmdBlitImage(&c, sl->image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, s->bgra, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &all, VK_FILTER_NEAREST);
        shown = s->bgra;
    }
    buffer_image(&c, IMAGE_TO_BUFFER, &b, shown, 1, &whole);
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

// ---- Vulkan 1.1 ----

static void unimplemented(const char *command);

// One group, of the one device.
static CALL VkResult CC EnumeratePhysicalDeviceGroups(VkInstance i, uint32_t *count, VkPhysicalDeviceGroupProperties *out) {
    if (open_device()) {
        *count = 0;
        return VK_SUCCESS;
    }
    if (!out) {
        *count = 1;
        return VK_SUCCESS;
    }
    if (*count < 1)
        return VK_INCOMPLETE;
    out[0].physicalDeviceCount = 1;
    memset(out[0].physicalDevices, 0, sizeof out[0].physicalDevices);
    out[0].physicalDevices[0] = &physical;
    out[0].subsetAllocation = VK_FALSE;
    *count = 1;
    return VK_SUCCESS;
}

static CALL void CC GetDeviceGroupPeerMemoryFeatures(VkDevice d, uint32_t heap, uint32_t local, uint32_t remote, VkPeerMemoryFeatureFlags *f) {
    *f = VK_PEER_MEMORY_FEATURE_COPY_SRC_BIT | VK_PEER_MEMORY_FEATURE_COPY_DST_BIT | VK_PEER_MEMORY_FEATURE_GENERIC_SRC_BIT | VK_PEER_MEMORY_FEATURE_GENERIC_DST_BIT;
}

static CALL void CC CmdSetDeviceMask(VkCommandBuffer c, uint32_t mask) {
}

// (From work group 0 only, as yet: a base would be the shaders' to add.)
static CALL void CC CmdDispatchBase(VkCommandBuffer c, uint32_t bx, uint32_t by, uint32_t bz, uint32_t x, uint32_t y, uint32_t z) {
    if (bx || by || bz) {
        unimplemented("vkCmdDispatchBase from a work group other than 0");
        c->failed = 1;
        return;
    }
    CmdDispatch(c, x, y, z);
}

// (Memory a buffer or image needs: never its own allocation.)
static void dedicated(void *chain) {
    for (VkBaseOutStructure *s = chain; s; s = s->pNext)
        if (s->sType == VK_STRUCTURE_TYPE_MEMORY_DEDICATED_REQUIREMENTS) {
            VkMemoryDedicatedRequirements *r = (VkMemoryDedicatedRequirements *)s;
            r->prefersDedicatedAllocation = r->requiresDedicatedAllocation = VK_FALSE;
        }
}

static CALL void CC GetBufferMemoryRequirements2(VkDevice d, const VkBufferMemoryRequirementsInfo2 *i, VkMemoryRequirements2 *r) {
    GetBufferMemoryRequirements(d, i->buffer, &r->memoryRequirements);
    dedicated(r->pNext);
}

static CALL void CC GetImageMemoryRequirements2(VkDevice d, const VkImageMemoryRequirementsInfo2 *i, VkMemoryRequirements2 *r) {
    GetImageMemoryRequirements(d, i->image, &r->memoryRequirements);
    dedicated(r->pNext);
}

static CALL void CC GetImageSparseMemoryRequirements2(VkDevice d, const VkImageSparseMemoryRequirementsInfo2 *i, uint32_t *count, VkSparseImageMemoryRequirements2 *r) {
    *count = 0;
}

static CALL VkResult CC BindBufferMemory2(VkDevice d, uint32_t n, const VkBindBufferMemoryInfo *b) {
    for (uint32_t i = 0; i < n; i++)
        BindBufferMemory(d, b[i].buffer, b[i].memory, b[i].memoryOffset);
    return VK_SUCCESS;
}

static CALL VkResult CC BindImageMemory2(VkDevice d, uint32_t n, const VkBindImageMemoryInfo *b) {
    for (uint32_t i = 0; i < n; i++)
        BindImageMemory(d, b[i].image, b[i].memory, b[i].memoryOffset);
    return VK_SUCCESS;
}

static CALL void CC TrimCommandPool(VkDevice d, VkCommandPool pool, VkCommandPoolTrimFlags flags) {
}

// (Unprotected queues only: the one.)
static CALL void CC GetDeviceQueue2(VkDevice d, const VkDeviceQueueInfo2 *i, VkQueue *q) {
    *q = i->flags ? VK_NULL_HANDLE : &d->queue;
}

// A descriptor update template: its entries, each a run of descriptors
// read from the program's data at an offset and stride, written as
// vkUpdateDescriptorSets would.
struct VkDescriptorUpdateTemplate_T {
    uint32_t n;
    VkDescriptorUpdateTemplateEntry *entries;
};

static CALL VkResult CC CreateDescriptorUpdateTemplate(VkDevice d, const VkDescriptorUpdateTemplateCreateInfo *info, const VkAllocationCallbacks *a, VkDescriptorUpdateTemplate *out) {
    struct VkDescriptorUpdateTemplate_T *t = zalloc(sizeof *t);
    if (!t || !(t->entries = zalloc(info->descriptorUpdateEntryCount * sizeof *t->entries))) {
        free(t);
        return VK_ERROR_OUT_OF_HOST_MEMORY;
    }
    t->n = info->descriptorUpdateEntryCount;
    memcpy(t->entries, info->pDescriptorUpdateEntries, t->n * sizeof *t->entries);
    *out = t;
    return VK_SUCCESS;
}

static CALL void CC DestroyDescriptorUpdateTemplate(VkDevice d, VkDescriptorUpdateTemplate t, const VkAllocationCallbacks *a) {
    if (!t)
        return;
    free(t->entries);
    free(t);
}

static CALL void CC UpdateDescriptorSetWithTemplate(VkDevice d, VkDescriptorSet set, VkDescriptorUpdateTemplate t, const void *data) {
    for (uint32_t i = 0; i < t->n; i++) {
        const VkDescriptorUpdateTemplateEntry *e = &t->entries[i];
        for (uint32_t k = 0; k < e->descriptorCount; k++) {
            const char *at = (const char *)data + e->offset + (size_t)k * e->stride;
            VkWriteDescriptorSet w = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, NULL, set, e->dstBinding, e->dstArrayElement + k, 1, e->descriptorType,
                                      (const VkDescriptorImageInfo *)at, (const VkDescriptorBufferInfo *)at, (const VkBufferView *)at};
            UpdateDescriptorSets(d, 1, &w, 0, NULL);
        }
    }
}

// (A set layout is supported if its descriptors are within a set's.)
static CALL void CC GetDescriptorSetLayoutSupport(VkDevice d, const VkDescriptorSetLayoutCreateInfo *info, VkDescriptorSetLayoutSupport *s) {
    uint32_t total = 0;
    for (uint32_t i = 0; i < info->bindingCount; i++)
        total += info->pBindings[i].descriptorCount;
    s->supported = total <= 1024;
}

// (No buffers, fences or semaphores shared with other APIs or processes.)
static CALL void CC GetPhysicalDeviceExternalBufferProperties(VkPhysicalDevice pd, const VkPhysicalDeviceExternalBufferInfo *i, VkExternalBufferProperties *p) {
    memset(&p->externalMemoryProperties, 0, sizeof p->externalMemoryProperties);
}

static CALL void CC GetPhysicalDeviceExternalFenceProperties(VkPhysicalDevice pd, const VkPhysicalDeviceExternalFenceInfo *i, VkExternalFenceProperties *p) {
    p->exportFromImportedHandleTypes = p->compatibleHandleTypes = 0;
    p->externalFenceFeatures = 0;
}

static CALL void CC GetPhysicalDeviceExternalSemaphoreProperties(VkPhysicalDevice pd, const VkPhysicalDeviceExternalSemaphoreInfo *i, VkExternalSemaphoreProperties *p) {
    p->exportFromImportedHandleTypes = p->compatibleHandleTypes = 0;
    p->externalSemaphoreFeatures = 0;
}

// ---- Entry points ----

long write(int, const void *, size_t);

// A command the driver doesn't implement, said once on stderr (from the
// generated stubs).
static void unimplemented(const char *command) {
    static const char *said[64];
    for (size_t k = 0; k < sizeof said / sizeof *said; k++) {
        if (said[k] == command)
            return;
        if (!said[k]) {
            said[k] = command;
            break;
        }
    }
    char line[128];
    int n = snprintf(line, sizeof line, "wasm-env's Vulkan driver: %s isn't implemented\n", command);
    if (n > 0)
        write(2, line, (size_t)n < sizeof line ? (size_t)n : sizeof line - 1);
}

API CALL PFN_vkVoidFunction CC vk_icdGetInstanceProcAddr(VkInstance i, const char *name);
static CALL PFN_vkVoidFunction CC GetDeviceProcAddr(VkDevice d, const char *name);

static CALL PFN_vkVoidFunction CC GetInstanceProcAddr(VkInstance i, const char *name) {
    return vk_icdGetInstanceProcAddr(i, name);
}

static CALL VkResult CC EnumerateInstanceLayerProperties(uint32_t *count, VkLayerProperties *out) {
    *count = 0;
    return VK_SUCCESS;
}

struct entry {
    const char *name;
    PFN_vkVoidFunction f;
    // The extension it's of (NULL: Vulkan's own).
    const char *extension;
};
// The tables, global, instance and device, from vk.xml (entries.awk).
#include "entries.h"

static const struct entry *lookup(const struct entry *t, size_t n, const char *name) {
    for (size_t k = 0; k < n; k++)
        if (!strcmp(t[k].name, name))
            return &t[k];
    return NULL;
}
#define LOOKUP(t, name) lookup(t, sizeof t / sizeof *t, name)
#define FIND(t, name) (LOOKUP(t, name) ? LOOKUP(t, name)->f : NULL)

// (An extension's commands only where the device has it enabled.)
static CALL PFN_vkVoidFunction CC GetDeviceProcAddr(VkDevice d, const char *name) {
    const struct entry *e = LOOKUP(device, name);
    if (!e)
        return NULL;
    if (e->extension) {
        size_t k = 0, n = sizeof device_extensions / sizeof *device_extensions;
        while (k < n && strcmp(device_extensions[k].extensionName, e->extension))
            k++;
        if (k == n || !(d->extensions >> k & 1))
            return NULL;
    }
    return e->f;
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
