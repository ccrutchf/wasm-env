// Vulkan compute through wasm-env's driver: a shader (vulkan.comp, as
// SPIR-V in vulkan_spv.h) over buffers in one mapped memory, with two
// descriptor sets, push constants, a fill, an update and a copy, waited for
// with a fence; then a write of the program's own to a buffer in that
// memory the submission doesn't use, made while the device works elsewhere
// in it, still there after the wait.
#include <stdio.h>
#include <string.h>
#include <vulkan/vulkan.h>
#include "vulkan_spv.h"
#include "vulkan_image_spv.h"

#define N 1000
#define CHECK(x)                                                    \
    do {                                                            \
        VkResult r_ = (x);                                          \
        if (r_ != VK_SUCCESS) {                                     \
            printf("FAILED: %s (%d)\n", #x, r_);                    \
            return 1;                                               \
        }                                                           \
    } while (0)

// A 16 × 16 RGBA8 image made from a buffer (rows of 64 bytes: not
// WebGPU's 256), read through a combined image sampler (vulkan_image.comp)
// into a storage image and a buffer; part of the storage image then
// overwritten from another, cleared, image; and the storage image read back
// into a buffer.
static int images(VkDevice dev, VkQueue queue) {
    VkFormat rgba = VK_FORMAT_R8G8B8A8_UNORM;
    VkImage im[3];
    VkImageUsageFlags usage[3] = {VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT, VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT,
                                  VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT};
    VkDeviceMemory imem[3];
    for (int k = 0; k < 3; k++) {
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = rgba, .extent = {16, 16, 1}, .mipLevels = 1, .arrayLayers = 1,
                                .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL, .usage = usage[k]};
        CHECK(vkCreateImage(dev, &ii, NULL, &im[k]));
        VkMemoryRequirements req;
        vkGetImageMemoryRequirements(dev, im[k], &req);
        VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size, .memoryTypeIndex = 0};
        CHECK(vkAllocateMemory(dev, &mi, NULL, &imem[k]));
        CHECK(vkBindImageMemory(dev, im[k], imem[k], 0));
    }
    // One memory for the buffers: the texels in, the fetched texels, and
    // the storage image read back.
    VkBuffer buf[3];
    VkDeviceSize at[3], total = 0;
    for (int k = 0; k < 3; k++) {
        VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = 16 * 16 * 4,
                                 .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT};
        CHECK(vkCreateBuffer(dev, &bi, NULL, &buf[k]));
        VkMemoryRequirements req;
        vkGetBufferMemoryRequirements(dev, buf[k], &req);
        at[k] = (total + req.alignment - 1) / req.alignment * req.alignment;
        total = at[k] + req.size;
    }
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = total, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    for (int k = 0; k < 3; k++)
        CHECK(vkBindBufferMemory(dev, buf[k], mem, at[k]));
    uint8_t *map;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&map));
    uint8_t *texels = map + at[0];
    uint32_t *fetched = (uint32_t *)(map + at[1]);
    uint8_t *back = map + at[2];
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++) {
            uint8_t *t = texels + (y * 16 + x) * 4;
            t[0] = x * 16, t[1] = y * 16, t[2] = x + y, t[3] = 255;
        }

    VkImageView view[2];
    for (int k = 0; k < 2; k++) {
        VkImageViewCreateInfo vi = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = im[k], .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = rgba,
                                    .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
        CHECK(vkCreateImageView(dev, &vi, NULL, &view[k]));
    }
    VkSamplerCreateInfo smi = {VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO, .magFilter = VK_FILTER_NEAREST, .minFilter = VK_FILTER_NEAREST,
                               .addressModeU = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .addressModeV = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
                               .addressModeW = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, .maxLod = 1};
    VkSampler sampler;
    CHECK(vkCreateSampler(dev, &smi, NULL, &sampler));

    VkShaderModuleCreateInfo si = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof image_spv, .pCode = image_spv};
    VkShaderModule shader;
    CHECK(vkCreateShaderModule(dev, &si, NULL, &shader));
    VkDescriptorSetLayoutBinding b[3] = {{0, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1, VK_SHADER_STAGE_COMPUTE_BIT},
                                         {1, VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, 1, VK_SHADER_STAGE_COMPUTE_BIT},
                                         {2, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT}};
    VkDescriptorSetLayoutCreateInfo li = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 3, .pBindings = b};
    VkDescriptorSetLayout sl;
    CHECK(vkCreateDescriptorSetLayout(dev, &li, NULL, &sl));
    VkPipelineLayoutCreateInfo pli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &sl};
    VkPipelineLayout layout;
    CHECK(vkCreatePipelineLayout(dev, &pli, NULL, &layout));
    VkComputePipelineCreateInfo ci = {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
                                      .stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = shader, .pName = "main"},
                                      .layout = layout};
    VkPipeline pipeline;
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &ci, NULL, &pipeline));
    VkDescriptorPoolSize ps[3] = {{VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 1}, {VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, 1}, {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1}};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 3, .pPoolSizes = ps};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = pool, .descriptorSetCount = 1, .pSetLayouts = &sl};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(dev, &dai, &set));
    VkDescriptorImageInfo sampled = {sampler, view[0], VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL}, storage = {VK_NULL_HANDLE, view[1], VK_IMAGE_LAYOUT_GENERAL};
    VkDescriptorBufferInfo bfetched = {buf[1], 0, VK_WHOLE_SIZE};
    VkWriteDescriptorSet w[3] = {
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, .pImageInfo = &sampled},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 1, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, .pImageInfo = &storage},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 2, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &bfetched},
    };
    vkUpdateDescriptorSets(dev, 3, w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    VkImageSubresourceLayers color = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    VkBufferImageCopy whole = {0, 0, 0, color, {0, 0, 0}, {16, 16, 1}};
    vkCmdCopyBufferToImage(cmd, buf[0], im[0], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &whole);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &set, 0, NULL);
    vkCmdDispatch(cmd, 2, 2, 1);
    VkClearColorValue clear = {.float32 = {0.25f, 0.5f, 0.75f, 1.0f}};
    VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    vkCmdClearColorImage(cmd, im[2], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &clear, 1, &range);
    VkImageCopy corner = {color, {4, 4, 0}, color, {0, 0, 0}, {8, 8, 1}};
    vkCmdCopyImage(cmd, im[2], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, im[1], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &corner);
    vkCmdCopyImageToBuffer(cmd, im[1], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf[2], 1, &whole);
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));

    int wrong_fetch = 0, wrong_image = 0;
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++) {
            uint8_t *t = texels + (y * 16 + x) * 4, *g = back + (y * 16 + x) * 4;
            uint32_t want = t[0] | t[1] << 8 | t[2] << 16 | (uint32_t)t[3] << 24;
            wrong_fetch += fetched[y * 16 + x] != want;
            // (The cleared corner, else the texel with red and blue swapped.)
            uint8_t w4[4] = {64, 128, 191, 255};
            if (x >= 8 || y >= 8)
                w4[0] = t[2], w4[1] = t[1], w4[2] = t[0], w4[3] = t[3];
            for (int k = 0; k < 4; k++)
                wrong_image += g[k] < w4[k] - 1 || g[k] > w4[k] + 1;
        }
    printf("images: texels fetched %d wrong, image %d wrong\n", wrong_fetch, wrong_image);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl, NULL);
    vkDestroyShaderModule(dev, shader, NULL);
    vkDestroySampler(dev, sampler, NULL);
    for (int k = 0; k < 2; k++)
        vkDestroyImageView(dev, view[k], NULL);
    for (int k = 0; k < 3; k++) {
        vkDestroyImage(dev, im[k], NULL);
        vkFreeMemory(dev, imem[k], NULL);
        vkDestroyBuffer(dev, buf[k], NULL);
    }
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

int main(void) {
    VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO, .apiVersion = VK_API_VERSION_1_0};
    VkInstanceCreateInfo ii = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance inst;
    CHECK(vkCreateInstance(&ii, NULL, &inst));
    uint32_t n = 1;
    VkPhysicalDevice pd;
    CHECK(vkEnumeratePhysicalDevices(inst, &n, &pd));
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(pd, &props);
    printf("device: %s\n", strstr(props.deviceName, "(wasm-env)") ? "wasm-env's" : props.deviceName);

    float prio = 1;
    VkDeviceQueueCreateInfo qi = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, .queueFamilyIndex = 0, .queueCount = 1, .pQueuePriorities = &prio};
    VkDeviceCreateInfo di = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .queueCreateInfoCount = 1, .pQueueCreateInfos = &qi};
    VkDevice dev;
    CHECK(vkCreateDevice(pd, &di, NULL, &dev));
    VkQueue queue;
    vkGetDeviceQueue(dev, 0, 0, &queue);

    // Six buffers in one memory: in, out, the uniform, a copy of out, a
    // filled one, and one the commands don't use.
    VkDeviceSize size[6] = {N * 4, N * 4, 16, N * 4, 64, 64}, at[6], total = 0;
    VkBuffer buf[6];
    VkBufferUsageFlags use = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    for (int k = 0; k < 6; k++) {
        VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = size[k], .usage = use};
        CHECK(vkCreateBuffer(dev, &bi, NULL, &buf[k]));
        VkMemoryRequirements req;
        vkGetBufferMemoryRequirements(dev, buf[k], &req);
        at[k] = (total + req.alignment - 1) / req.alignment * req.alignment;
        total = at[k] + req.size;
    }
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = total, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    for (int k = 0; k < 6; k++)
        CHECK(vkBindBufferMemory(dev, buf[k], mem, at[k]));
    uint8_t *map;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&map));
    uint32_t *in = (uint32_t *)(map + at[0]), *out = (uint32_t *)(map + at[1]), *copy = (uint32_t *)(map + at[3]), *filled = (uint32_t *)(map + at[4]), *unused = (uint32_t *)(map + at[5]);
    for (uint32_t i = 0; i < N; i++)
        in[i] = i * 7;
    *(uint32_t *)(map + at[2]) = 11;

    VkShaderModuleCreateInfo si = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof compute_spv, .pCode = compute_spv};
    VkShaderModule shader;
    CHECK(vkCreateShaderModule(dev, &si, NULL, &shader));
    VkDescriptorSetLayoutBinding b0[2] = {{0, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT}, {1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT}};
    VkDescriptorSetLayoutBinding b1 = {0, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT};
    VkDescriptorSetLayoutCreateInfo li0 = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 2, .pBindings = b0};
    VkDescriptorSetLayoutCreateInfo li1 = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &b1};
    VkDescriptorSetLayout sl[2];
    CHECK(vkCreateDescriptorSetLayout(dev, &li0, NULL, &sl[0]));
    CHECK(vkCreateDescriptorSetLayout(dev, &li1, NULL, &sl[1]));
    VkPushConstantRange pr = {VK_SHADER_STAGE_COMPUTE_BIT, 0, 8};
    VkPipelineLayoutCreateInfo pli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 2, .pSetLayouts = sl, .pushConstantRangeCount = 1, .pPushConstantRanges = &pr};
    VkPipelineLayout layout;
    CHECK(vkCreatePipelineLayout(dev, &pli, NULL, &layout));
    VkComputePipelineCreateInfo ci = {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
                                      .stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = shader, .pName = "main"},
                                      .layout = layout};
    VkPipeline pipeline;
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &ci, NULL, &pipeline));

    VkDescriptorPoolSize ps[2] = {{VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 2}, {VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1}};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 2, .poolSizeCount = 2, .pPoolSizes = ps};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = pool, .descriptorSetCount = 2, .pSetLayouts = sl};
    VkDescriptorSet sets[2];
    CHECK(vkAllocateDescriptorSets(dev, &dai, sets));
    VkDescriptorBufferInfo bin = {buf[0], 0, VK_WHOLE_SIZE}, bout = {buf[1], 0, VK_WHOLE_SIZE}, buni = {buf[2], 0, 16};
    VkWriteDescriptorSet w[3] = {
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = sets[0], .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &bin},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = sets[0], .dstBinding = 1, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &bout},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = sets[1], .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, .pBufferInfo = &buni},
    };
    vkUpdateDescriptorSets(dev, 3, w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 2, sets, 0, NULL);
    uint32_t pc[2] = {3, N};
    vkCmdPushConstants(cmd, layout, VK_SHADER_STAGE_COMPUTE_BIT, 0, 8, pc);
    vkCmdDispatch(cmd, (N + 63) / 64, 1, 1);
    VkMemoryBarrier mb = {VK_STRUCTURE_TYPE_MEMORY_BARRIER, .srcAccessMask = VK_ACCESS_SHADER_WRITE_BIT, .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT};
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &mb, 0, NULL, 0, NULL);
    VkBufferCopy region = {0, 0, N * 4};
    vkCmdCopyBuffer(cmd, buf[1], buf[3], 1, &region);
    vkCmdFillBuffer(cmd, buf[4], 0, VK_WHOLE_SIZE, 0x5a5a5a5a);
    uint32_t marks[2] = {0xc0ffee, 0xbeef};
    vkCmdUpdateBuffer(cmd, buf[4], 8, 8, marks);
    CHECK(vkEndCommandBuffer(cmd));

    VkFenceCreateInfo fi = {VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence;
    CHECK(vkCreateFence(dev, &fi, NULL, &fence));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, fence));
    // (The program's own write, to a buffer the submission doesn't use.)
    unused[0] = 12345;
    CHECK(vkWaitForFences(dev, 1, &fence, VK_TRUE, ~0ull));

    int wrong = 0;
    for (uint32_t i = 0; i < N; i++)
        wrong += out[i] != i * 7 * 3 + 11 || copy[i] != out[i];
    printf("compute: %d of %d wrong\n", wrong, N);
    printf("fill and update: %s\n", filled[0] == 0x5a5a5a5a && filled[2] == 0xc0ffee && filled[3] == 0xbeef && filled[15] == 0x5a5a5a5a ? "right" : "wrong");
    printf("the program's own write kept: %s\n", unused[0] == 12345 ? "yes" : "no");
    if (images(dev, queue))
        return 1;

    vkDestroyFence(dev, fence, NULL);
    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl[0], NULL);
    vkDestroyDescriptorSetLayout(dev, sl[1], NULL);
    vkDestroyShaderModule(dev, shader, NULL);
    vkUnmapMemory(dev, mem);
    for (int k = 0; k < 6; k++)
        vkDestroyBuffer(dev, buf[k], NULL);
    vkFreeMemory(dev, mem, NULL);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
    puts("done");
    return 0;
}
