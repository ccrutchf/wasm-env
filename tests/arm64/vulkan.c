// Vulkan compute through wasm-env's driver: a shader (vulkan.comp, as
// SPIR-V in vulkan_spv.h, specialized) over buffers in one mapped memory,
// with two descriptor sets, push constants, a fill, an update and a copy,
// waited for with a fence; then a write of the program's own to a buffer
// in that memory the submission doesn't use, made while the device works
// elsewhere in it, still there after the wait.
#include <stdio.h>
#include <string.h>
#include <vulkan/vulkan.h>
#include "vulkan_spv.h"
#include "vulkan_image_spv.h"
#include "vulkan_draw_frag.h"
#include "vulkan_draw_vert.h"
#include "vulkan_subpass_frag.h"
#include "vulkan_subpass_vert.h"
#include "vulkan_texel_spv.h"
#include "vulkan_array_spv.h"
#include "vulkan_fan_vert.h"
#include "vulkan_fan_frag.h"
#include "vulkan_texel_write_spv.h"
#include "vulkan_layout_spv.h"

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

// A descriptor array: three 1 × 1 images cleared to colors of their own,
// written into one binding's three elements (one write, from element 0),
// and read by a compute shader (vulkan_array.comp) into words.
static int arrays(VkDevice dev, VkQueue queue) {
    VkImage im[3];
    VkImageView view[3];
    for (int k = 0; k < 3; k++) {
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM, .extent = {1, 1, 1}, .mipLevels = 1,
                                .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
                                .usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT};
        CHECK(vkCreateImage(dev, &ii, NULL, &im[k]));
        VkImageViewCreateInfo vi = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = im[k], .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM,
                                    .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
        CHECK(vkCreateImageView(dev, &vi, NULL, &view[k]));
    }
    VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = 16, .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkBuffer buf;
    CHECK(vkCreateBuffer(dev, &bi, NULL, &buf));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(dev, buf, &req);
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    CHECK(vkBindBufferMemory(dev, buf, mem, 0));
    uint32_t *out;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&out));

    VkShaderModuleCreateInfo si = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof array_spv, .pCode = array_spv};
    VkShaderModule shader;
    CHECK(vkCreateShaderModule(dev, &si, NULL, &shader));
    VkDescriptorSetLayoutBinding b[2] = {{0, VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, 3, VK_SHADER_STAGE_COMPUTE_BIT}, {1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT}};
    VkDescriptorSetLayoutCreateInfo li = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 2, .pBindings = b};
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
    VkDescriptorPoolSize ps[2] = {{VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, 3}, {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1}};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 2, .pPoolSizes = ps};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = pool, .descriptorSetCount = 1, .pSetLayouts = &sl};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(dev, &dai, &set));
    VkDescriptorImageInfo images[3];
    for (int k = 0; k < 3; k++)
        images[k] = (VkDescriptorImageInfo){VK_NULL_HANDLE, view[k], VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkDescriptorBufferInfo bo = {buf, 0, VK_WHOLE_SIZE};
    VkWriteDescriptorSet w[2] = {
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 0, .dstArrayElement = 0, .descriptorCount = 3, .descriptorType = VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE,
         .pImageInfo = images},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 1, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &bo},
    };
    vkUpdateDescriptorSets(dev, 2, w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    const float colors[3][4] = {{1, 0, 0, 1}, {0, 1, 0, 0.5f}, {0, 0, 1, 0}};
    VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    for (int k = 0; k < 3; k++) {
        VkClearColorValue c = {.float32 = {colors[k][0], colors[k][1], colors[k][2], colors[k][3]}};
        vkCmdClearColorImage(cmd, im[k], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &c, 1, &range);
    }
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &set, 0, NULL);
    vkCmdDispatch(cmd, 1, 1, 1);
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));
    const uint32_t want[3] = {0xff0000ffu, 0x8000ff00u, 0x00ff0000u};
    int wrong = 0;
    for (int k = 0; k < 3; k++)
        for (int c = 0; c < 32; c += 8) {
            int got = out[k] >> c & 255, w = want[k] >> c & 255;
            wrong += got < w - 1 || got > w + 1;
        }
    printf("descriptor array: %d of 12 wrong\n", wrong);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl, NULL);
    vkDestroyShaderModule(dev, shader, NULL);
    for (int k = 0; k < 3; k++) {
        vkDestroyImageView(dev, view[k], NULL);
        vkDestroyImage(dev, im[k], NULL);
    }
    vkDestroyBuffer(dev, buf, NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

// A texel buffer: 256 RGBA8 texels the program writes into mapped memory,
// viewed as a uniform texel buffer (a VkBufferView) and read by a compute
// shader (vulkan_texel.comp) into words, which come back as written.
static int texels(VkDevice dev, VkQueue queue) {
    VkBuffer buf[2];
    VkBufferUsageFlags use[2] = {VK_BUFFER_USAGE_UNIFORM_TEXEL_BUFFER_BIT, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkDeviceSize at[2], total = 0;
    for (int k = 0; k < 2; k++) {
        VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = 1024, .usage = use[k]};
        CHECK(vkCreateBuffer(dev, &bi, NULL, &buf[k]));
        VkMemoryRequirements req;
        vkGetBufferMemoryRequirements(dev, buf[k], &req);
        at[k] = (total + req.alignment - 1) / req.alignment * req.alignment;
        total = at[k] + req.size;
    }
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = total, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    for (int k = 0; k < 2; k++)
        CHECK(vkBindBufferMemory(dev, buf[k], mem, at[k]));
    uint8_t *map;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&map));
    uint32_t *in = (uint32_t *)(map + at[0]), *out = (uint32_t *)(map + at[1]);
    for (uint32_t i = 0; i < 256; i++)
        in[i] = i | (255 - i) << 8 | (i * 7 & 255) << 16 | 0xffu << 24;

    VkBufferViewCreateInfo bvi = {VK_STRUCTURE_TYPE_BUFFER_VIEW_CREATE_INFO, .buffer = buf[0], .format = VK_FORMAT_R8G8B8A8_UNORM, .offset = 0, .range = VK_WHOLE_SIZE};
    VkBufferView view;
    CHECK(vkCreateBufferView(dev, &bvi, NULL, &view));
    VkShaderModuleCreateInfo si = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof texel_spv, .pCode = texel_spv};
    VkShaderModule shader;
    CHECK(vkCreateShaderModule(dev, &si, NULL, &shader));
    VkDescriptorSetLayoutBinding b[2] = {{0, VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT},
                                         {1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT}};
    VkDescriptorSetLayoutCreateInfo li = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 2, .pBindings = b};
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
    VkDescriptorPoolSize ps[2] = {{VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER, 1}, {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1}};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 2, .pPoolSizes = ps};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = pool, .descriptorSetCount = 1, .pSetLayouts = &sl};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(dev, &dai, &set));
    VkDescriptorBufferInfo bo = {buf[1], 0, VK_WHOLE_SIZE};
    VkWriteDescriptorSet w[2] = {
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER, .pTexelBufferView = &view},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 1, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &bo},
    };
    vkUpdateDescriptorSets(dev, 2, w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &set, 0, NULL);
    vkCmdDispatch(cmd, 4, 1, 1);
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));
    int wrong = 0;
    for (int i = 0; i < 256; i++)
        wrong += out[i] != in[i];
    printf("texel buffer: %d of 256 wrong\n", wrong);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl, NULL);
    vkDestroyShaderModule(dev, shader, NULL);
    vkDestroyBufferView(dev, view, NULL);
    for (int k = 0; k < 2; k++)
        vkDestroyBuffer(dev, buf[k], NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

// Layouts WGSL can't say (vulkan_layout.comp): a std140 block's float
// array, mat2 and row-major mat2x3, read whole and a component at a time;
// a texel buffer of 70006 R32F texels, its last six read; a descriptor
// array of two blocks; and gl_WorkGroupSize from a specialization
// constant (6). 29 floats come back.
static int layouts(VkDevice dev, VkQueue queue) {
    enum { TEXELS = 70006 };
    VkDeviceSize size[5] = {144, 29 * 4, TEXELS * 4, 4, 4}, at[5], total = 0;
    VkBufferUsageFlags use[5] = {VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, VK_BUFFER_USAGE_UNIFORM_TEXEL_BUFFER_BIT,
                                 VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT};
    VkBuffer buf[5];
    for (int k = 0; k < 5; k++) {
        VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = size[k], .usage = use[k]};
        CHECK(vkCreateBuffer(dev, &bi, NULL, &buf[k]));
        VkMemoryRequirements req;
        vkGetBufferMemoryRequirements(dev, buf[k], &req);
        at[k] = (total + req.alignment - 1) / req.alignment * req.alignment;
        total = at[k] + req.size;
    }
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = total, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    for (int k = 0; k < 5; k++)
        CHECK(vkBindBufferMemory(dev, buf[k], mem, at[k]));
    uint8_t *map;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&map));
    // The block: f[k] at 16k; m's column c, row r at 64 + 16c + 4r; r's
    // (row-major) at 96 + 16 × row + 4 × column. Each float its offset.
    float *u = (float *)(map + at[0]), *out = (float *)(map + at[1]), *big = (float *)(map + at[2]);
    for (int k = 0; k < 36; k++)
        u[k] = (float)(4 * k);
    for (int k = 0; k < TEXELS; k++)
        big[k] = (float)k;
    *(float *)(map + at[3]) = 0.5f;
    *(float *)(map + at[4]) = 1.5f;
    float want[29];
    for (int i = 0; i < 6; i++) {
        if (i < 4) {
            want[i] = 16 * i;
            want[4 + i] = 64 + 16 * (i / 2) + 4 * (i % 2);
        }
        want[8 + i] = want[14 + i] = 96 + 16 * (i % 3) + 4 * (i / 3);
        want[20 + i] = 70000 + i;
    }
    want[26] = 0.5f, want[27] = 1.5f, want[28] = 6;

    VkBufferViewCreateInfo bvi = {VK_STRUCTURE_TYPE_BUFFER_VIEW_CREATE_INFO, .buffer = buf[2], .format = VK_FORMAT_R32_SFLOAT, .offset = 0, .range = VK_WHOLE_SIZE};
    VkBufferView view;
    CHECK(vkCreateBufferView(dev, &bvi, NULL, &view));
    VkShaderModuleCreateInfo si = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof layout_spv, .pCode = layout_spv};
    VkShaderModule shader;
    CHECK(vkCreateShaderModule(dev, &si, NULL, &shader));
    VkDescriptorSetLayoutBinding b[4] = {{0, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT},
                                         {1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT},
                                         {2, VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT},
                                         {3, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 2, VK_SHADER_STAGE_COMPUTE_BIT}};
    VkDescriptorSetLayoutCreateInfo li = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 4, .pBindings = b};
    VkDescriptorSetLayout sl;
    CHECK(vkCreateDescriptorSetLayout(dev, &li, NULL, &sl));
    VkPipelineLayoutCreateInfo pli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &sl};
    VkPipelineLayout layout;
    CHECK(vkCreatePipelineLayout(dev, &pli, NULL, &layout));
    uint32_t six = 6;
    VkSpecializationMapEntry entry = {0, 0, 4};
    VkSpecializationInfo spec = {1, &entry, 4, &six};
    VkComputePipelineCreateInfo ci = {
        VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = shader, .pName = "main", .pSpecializationInfo = &spec},
        .layout = layout};
    VkPipeline pipeline;
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &ci, NULL, &pipeline));
    VkDescriptorPoolSize ps[3] = {{VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1}, {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 3}, {VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER, 1}};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 3, .pPoolSizes = ps};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = pool, .descriptorSetCount = 1, .pSetLayouts = &sl};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(dev, &dai, &set));
    VkDescriptorBufferInfo bo[4] = {{buf[0], 0, VK_WHOLE_SIZE}, {buf[1], 0, VK_WHOLE_SIZE}, {buf[3], 0, VK_WHOLE_SIZE}, {buf[4], 0, VK_WHOLE_SIZE}};
    VkWriteDescriptorSet w[4] = {
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, .pBufferInfo = &bo[0]},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 1, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &bo[1]},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 2, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_UNIFORM_TEXEL_BUFFER, .pTexelBufferView = &view},
        {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 3, .descriptorCount = 2, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &bo[2]},
    };
    vkUpdateDescriptorSets(dev, 4, w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &set, 0, NULL);
    vkCmdDispatch(cmd, 1, 1, 1);
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));
    int wrong = 0;
    for (int i = 0; i < 29; i++)
        wrong += out[i] != want[i];
    printf("layouts: %d of 29 wrong\n", wrong);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl, NULL);
    vkDestroyShaderModule(dev, shader, NULL);
    vkDestroyBufferView(dev, view, NULL);
    for (int k = 0; k < 5; k++)
        vkDestroyBuffer(dev, buf[k], NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

// Two subpasses: the first clears an 8 × 8 attachment (A) to a color; the
// second reads it as an input attachment (vulkan_subpass.frag) and writes
// its inverse to another (B), which is read back. A's store op says not to
// keep it: it's kept until the second subpass has read it.
static int subpasses(VkDevice dev, VkQueue queue) {
    enum { W = 8 };
    VkFormat rgba = VK_FORMAT_R8G8B8A8_UNORM;
    VkImageUsageFlags usage[2] = {VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_INPUT_ATTACHMENT_BIT,
                                  VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT};
    VkImage im[2];
    VkImageView view[2];
    for (int k = 0; k < 2; k++) {
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = rgba, .extent = {W, W, 1}, .mipLevels = 1, .arrayLayers = 1,
                                .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL, .usage = usage[k]};
        CHECK(vkCreateImage(dev, &ii, NULL, &im[k]));
        VkImageViewCreateInfo vi = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = im[k], .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = rgba,
                                    .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
        CHECK(vkCreateImageView(dev, &vi, NULL, &view[k]));
    }
    VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = W * W * 4, .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf;
    CHECK(vkCreateBuffer(dev, &bi, NULL, &buf));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(dev, buf, &req);
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    CHECK(vkBindBufferMemory(dev, buf, mem, 0));
    uint8_t *px;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&px));

    VkAttachmentDescription att[2] = {
        {0, rgba, VK_SAMPLE_COUNT_1_BIT, VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_DONT_CARE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL},
        {0, rgba, VK_SAMPLE_COUNT_1_BIT, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL},
    };
    VkAttachmentReference a_out = {0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL}, a_in = {0, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL},
                          b_out = {1, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub[2] = {
        {0, VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1, .pColorAttachments = &a_out},
        {0, VK_PIPELINE_BIND_POINT_GRAPHICS, .inputAttachmentCount = 1, .pInputAttachments = &a_in, .colorAttachmentCount = 1, .pColorAttachments = &b_out},
    };
    VkSubpassDependency dep = {0, 1, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_FRAGMENT_SHADER_BIT, VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                               VK_ACCESS_INPUT_ATTACHMENT_READ_BIT, VK_DEPENDENCY_BY_REGION_BIT};
    VkRenderPassCreateInfo rpi = {VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 2, .pAttachments = att, .subpassCount = 2, .pSubpasses = sub,
                                  .dependencyCount = 1, .pDependencies = &dep};
    VkRenderPass pass;
    CHECK(vkCreateRenderPass(dev, &rpi, NULL, &pass));
    VkFramebufferCreateInfo fbi = {VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = pass, .attachmentCount = 2, .pAttachments = view, .width = W, .height = W, .layers = 1};
    VkFramebuffer fb;
    CHECK(vkCreateFramebuffer(dev, &fbi, NULL, &fb));

    VkShaderModule shaders[2];
    VkShaderModuleCreateInfo si[2] = {{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof subpass_vert_spv, .pCode = subpass_vert_spv},
                                      {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof subpass_frag_spv, .pCode = subpass_frag_spv}};
    for (int k = 0; k < 2; k++)
        CHECK(vkCreateShaderModule(dev, &si[k], NULL, &shaders[k]));
    VkDescriptorSetLayoutBinding b = {0, VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT, 1, VK_SHADER_STAGE_FRAGMENT_BIT};
    VkDescriptorSetLayoutCreateInfo li = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &b};
    VkDescriptorSetLayout sl;
    CHECK(vkCreateDescriptorSetLayout(dev, &li, NULL, &sl));
    VkPipelineLayoutCreateInfo pli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &sl};
    VkPipelineLayout layout;
    CHECK(vkCreatePipelineLayout(dev, &pli, NULL, &layout));
    VkPipelineShaderStageCreateInfo stages[2] = {
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = shaders[0], .pName = "main"},
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = shaders[1], .pName = "main"},
    };
    VkPipelineVertexInputStateCreateInfo vis = {VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
    VkPipelineInputAssemblyStateCreateInfo ias = {VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkViewport viewport = {0, 0, W, W, 0, 1};
    VkRect2D scissor = {{0, 0}, {W, W}};
    VkPipelineViewportStateCreateInfo vps = {VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, .viewportCount = 1, .pViewports = &viewport, .scissorCount = 1, .pScissors = &scissor};
    VkPipelineRasterizationStateCreateInfo rs = {VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE, .lineWidth = 1};
    VkPipelineMultisampleStateCreateInfo ms = {VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT};
    VkPipelineColorBlendAttachmentState cba = {.colorWriteMask = 0xf};
    VkPipelineColorBlendStateCreateInfo cbs = {VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, .attachmentCount = 1, .pAttachments = &cba};
    VkGraphicsPipelineCreateInfo gpi = {VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
                                        .stageCount = 2,
                                        .pStages = stages,
                                        .pVertexInputState = &vis,
                                        .pInputAssemblyState = &ias,
                                        .pViewportState = &vps,
                                        .pRasterizationState = &rs,
                                        .pMultisampleState = &ms,
                                        .pColorBlendState = &cbs,
                                        .layout = layout,
                                        .renderPass = pass,
                                        .subpass = 1};
    VkPipeline pipeline;
    CHECK(vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gpi, NULL, &pipeline));
    VkDescriptorPoolSize ps = {VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT, 1};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &ps};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = pool, .descriptorSetCount = 1, .pSetLayouts = &sl};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(dev, &dai, &set));
    VkDescriptorImageInfo input = {VK_NULL_HANDLE, view[0], VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    VkWriteDescriptorSet w = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_INPUT_ATTACHMENT, .pImageInfo = &input};
    vkUpdateDescriptorSets(dev, 1, &w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    VkClearValue clears[2] = {{.color = {.float32 = {0.25f, 0.5f, 0.75f, 1}}}, {.color = {.float32 = {0, 0, 0, 0}}}};
    VkRenderPassBeginInfo rbi = {VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = pass, .framebuffer = fb, .renderArea = {{0, 0}, {W, W}}, .clearValueCount = 2, .pClearValues = clears};
    vkCmdBeginRenderPass(cmd, &rbi, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdNextSubpass(cmd, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, layout, 0, 1, &set, 0, NULL);
    vkCmdDraw(cmd, 3, 1, 0, 0);
    vkCmdEndRenderPass(cmd);
    VkBufferImageCopy whole = {0, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {W, W, 1}};
    vkCmdCopyImageToBuffer(cmd, im[1], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &whole);
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));

    int wrong = 0;
    const uint8_t want[4] = {191, 128, 64, 255};
    for (int k = 0; k < W * W; k++)
        for (int c = 0; c < 4; c++)
            wrong += px[4 * k + c] < want[c] - 1 || px[4 * k + c] > want[c] + 1;
    printf("subpasses: %d of %d wrong\n", wrong, W * W * 4);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl, NULL);
    for (int k = 0; k < 2; k++) {
        vkDestroyShaderModule(dev, shaders[k], NULL);
        vkDestroyImageView(dev, view[k], NULL);
        vkDestroyImage(dev, im[k], NULL);
    }
    vkDestroyFramebuffer(dev, fb, NULL);
    vkDestroyRenderPass(dev, pass, NULL);
    vkDestroyBuffer(dev, buf, NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

// A blit: a 16 × 16 RGBA8 image of four colored quadrants, linearly
// filtered into its 8 × 8 mip level, mirrored left to right, read back.
static int blits(VkDevice dev, VkQueue queue) {
    VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM, .extent = {16, 16, 1}, .mipLevels = 2,
                            .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
                            .usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT};
    VkImage im;
    CHECK(vkCreateImage(dev, &ii, NULL, &im));
    VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = 16 * 16 * 4, .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf;
    CHECK(vkCreateBuffer(dev, &bi, NULL, &buf));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(dev, buf, &req);
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    CHECK(vkBindBufferMemory(dev, buf, mem, 0));
    uint8_t *px;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&px));
    static const uint8_t quadrant[4][4] = {{255, 0, 0, 255}, {0, 255, 0, 255}, {0, 0, 255, 255}, {255, 255, 255, 255}};
    for (int y = 0; y < 16; y++)
        for (int x = 0; x < 16; x++)
            memcpy(px + (y * 16 + x) * 4, quadrant[(x >= 8) + 2 * (y >= 8)], 4);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    VkBufferImageCopy level0 = {0, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {16, 16, 1}};
    vkCmdCopyBufferToImage(cmd, buf, im, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &level0);
    VkImageBlit region = {{VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {{0, 0, 0}, {16, 16, 1}}, {VK_IMAGE_ASPECT_COLOR_BIT, 1, 0, 1}, {{8, 0, 0}, {0, 8, 1}}};
    vkCmdBlitImage(cmd, im, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, im, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region, VK_FILTER_LINEAR);
    VkBufferImageCopy level1 = {0, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 1, 0, 1}, {0, 0, 0}, {8, 8, 1}};
    vkCmdCopyImageToBuffer(cmd, im, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &level1);
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));

    int wrong = 0;
    for (int y = 0; y < 8; y++)
        for (int x = 0; x < 8; x++)
            wrong += memcmp(px + (y * 8 + x) * 4, quadrant[(x < 4) + 2 * (y >= 4)], 4) != 0;
    printf("blit: %d of 64 wrong\n", wrong);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyImage(dev, im, NULL);
    vkDestroyBuffer(dev, buf, NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

// Clears and resolves: an 8 × 8 RGBA8 image of four samples, cleared red
// as a render pass begins, a 4 × 4 square in its middle cleared green inside
// the pass (vkCmdClearAttachments), resolved into another image as the pass
// ends and into a third by vkCmdResolveImage; and a depth image cleared to
// 0.25 (vkCmdClearDepthStencilImage). All read back.
static int clears(VkDevice dev, VkPhysicalDevice pd, VkQueue queue) {
    enum { W = 8 };
    VkFormat rgba = VK_FORMAT_R8G8B8A8_UNORM;
    VkImageFormatProperties fp;
    CHECK(vkGetPhysicalDeviceImageFormatProperties(pd, rgba, VK_IMAGE_TYPE_2D, VK_IMAGE_TILING_OPTIMAL, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT, 0, &fp));
    if (!(fp.sampleCounts & VK_SAMPLE_COUNT_4_BIT)) {
        printf("clears: no four samples\n");
        return 1;
    }
    VkSampleCountFlagBits samples[3] = {VK_SAMPLE_COUNT_4_BIT, VK_SAMPLE_COUNT_1_BIT, VK_SAMPLE_COUNT_1_BIT};
    VkImageUsageFlags usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    VkImage im[3];
    VkImageView view[2];
    for (int k = 0; k < 3; k++) {
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = rgba, .extent = {W, W, 1}, .mipLevels = 1, .arrayLayers = 1,
                                .samples = samples[k], .tiling = VK_IMAGE_TILING_OPTIMAL, .usage = usage};
        CHECK(vkCreateImage(dev, &ii, NULL, &im[k]));
        if (k < 2) {
            VkImageViewCreateInfo vi = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = im[k], .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = rgba,
                                        .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
            CHECK(vkCreateImageView(dev, &vi, NULL, &view[k]));
        }
    }
    VkImageCreateInfo di = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_D32_SFLOAT, .extent = {W, W, 1}, .mipLevels = 1,
                            .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
                            .usage = VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT};
    VkImage depth;
    CHECK(vkCreateImage(dev, &di, NULL, &depth));
    VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = 3 * W * W * 4, .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf;
    CHECK(vkCreateBuffer(dev, &bi, NULL, &buf));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(dev, buf, &req);
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    CHECK(vkBindBufferMemory(dev, buf, mem, 0));
    uint8_t *px;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&px));

    VkAttachmentDescription att[2] = {
        {0, rgba, VK_SAMPLE_COUNT_4_BIT, VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL},
        {0, rgba, VK_SAMPLE_COUNT_1_BIT, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL},
    };
    VkAttachmentReference color = {0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL}, resolve = {1, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub = {0, VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1, .pColorAttachments = &color, .pResolveAttachments = &resolve};
    VkRenderPassCreateInfo rpi = {VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 2, .pAttachments = att, .subpassCount = 1, .pSubpasses = &sub};
    VkRenderPass pass;
    CHECK(vkCreateRenderPass(dev, &rpi, NULL, &pass));
    VkFramebufferCreateInfo fbi = {VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = pass, .attachmentCount = 2, .pAttachments = view, .width = W, .height = W, .layers = 1};
    VkFramebuffer fb;
    CHECK(vkCreateFramebuffer(dev, &fbi, NULL, &fb));

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    VkClearValue red = {.color = {.float32 = {1, 0, 0, 1}}};
    VkRenderPassBeginInfo rbi = {VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = pass, .framebuffer = fb, .renderArea = {{0, 0}, {W, W}}, .clearValueCount = 1, .pClearValues = &red};
    vkCmdBeginRenderPass(cmd, &rbi, VK_SUBPASS_CONTENTS_INLINE);
    VkClearAttachment green = {VK_IMAGE_ASPECT_COLOR_BIT, 0, {.color = {.float32 = {0, 1, 0, 1}}}};
    VkClearRect middle = {{{2, 2}, {4, 4}}, 0, 1};
    vkCmdClearAttachments(cmd, 1, &green, 1, &middle);
    vkCmdEndRenderPass(cmd);
    VkImageResolve whole = {{VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {W, W, 1}};
    vkCmdResolveImage(cmd, im[0], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, im[2], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &whole);
    VkClearDepthStencilValue quarter = {0.25f, 0};
    VkImageSubresourceRange depth_range = {VK_IMAGE_ASPECT_DEPTH_BIT, 0, 1, 0, 1};
    vkCmdClearDepthStencilImage(cmd, depth, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &quarter, 1, &depth_range);
    for (int k = 0; k < 3; k++) {
        VkBufferImageCopy copy = {k * W * W * 4, 0, 0, {k < 2 ? VK_IMAGE_ASPECT_COLOR_BIT : VK_IMAGE_ASPECT_DEPTH_BIT, 0, 0, 1}, {0, 0, 0}, {W, W, 1}};
        vkCmdCopyImageToBuffer(cmd, k == 0 ? im[1] : k == 1 ? im[2] : depth, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &copy);
    }
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));

    int wrong = 0;
    for (int k = 0; k < 2; k++)
        for (int y = 0; y < W; y++)
            for (int x = 0; x < W; x++) {
                int inside = x >= 2 && x < 6 && y >= 2 && y < 6;
                const uint8_t *p = px + k * W * W * 4 + (y * W + x) * 4;
                wrong += p[0] != (inside ? 0 : 255) || p[1] != (inside ? 255 : 0) || p[2] != 0 || p[3] != 255;
            }
    float d[W * W];
    memcpy(d, px + 2 * W * W * 4, sizeof d);
    for (int k = 0; k < W * W; k++)
        wrong += d[k] != 0.25f;
    printf("clears and resolves: %d of %d wrong\n", wrong, 3 * W * W);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyFramebuffer(dev, fb, NULL);
    vkDestroyRenderPass(dev, pass, NULL);
    for (int k = 0; k < 3; k++) {
        if (k < 2)
            vkDestroyImageView(dev, view[k], NULL);
        vkDestroyImage(dev, im[k], NULL);
    }
    vkDestroyImage(dev, depth, NULL);
    vkDestroyBuffer(dev, buf, NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

// What WebGPU lacks that the driver makes up for, read back:
// - a render pass that clears only its render area (the left half of an
//   8 × 8 image cleared green before: red there, green right);
// - triangle fans (vulkan_fan.vert, .frag: a quad), drawn and drawn
//   indexed, each into an image of its own: all blue;
// - a stencil write mask left to commands: stencil REPLACEd with 0xff
//   through a mask of 0x0f, read back 0x0f;
// - an occlusion query begun before the first fan's render pass and ended
//   after it: 64 samples;
// - a storage texel buffer written (vulkan_texel_write.comp): 256 words;
// - BC1 blocks copied into an image and out again, where the device has
//   them;
// - a device made with a feature it has and refused one it hasn't.
static int gaps(VkPhysicalDevice pd, VkDevice dev, VkQueue queue) {
    enum { W = 8 };
    VkFormat rgba = VK_FORMAT_R8G8B8A8_UNORM, ds = VK_FORMAT_D24_UNORM_S8_UINT;
    VkPhysicalDeviceFeatures have;
    vkGetPhysicalDeviceFeatures(pd, &have);
    VkPhysicalDeviceFeatures want = {.samplerAnisotropy = VK_TRUE}, not = {.geometryShader = VK_TRUE};
    float prio = 1;
    VkDeviceQueueCreateInfo qi = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, .queueFamilyIndex = 0, .queueCount = 1, .pQueuePriorities = &prio};
    VkDeviceCreateInfo di = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .queueCreateInfoCount = 1, .pQueueCreateInfos = &qi, .pEnabledFeatures = &want};
    VkDevice other;
    int features = have.samplerAnisotropy && vkCreateDevice(pd, &di, NULL, &other) == VK_SUCCESS;
    if (features)
        vkDestroyDevice(other, NULL);
    di.pEnabledFeatures = &not;
    features &= vkCreateDevice(pd, &di, NULL, &other) == VK_ERROR_FEATURE_NOT_PRESENT;

    // Images: one for the render area, two for fans (with a depth and
    // stencil image each), and their views.
    VkImage im[5];
    VkImageView view[5];
    for (int k = 0; k < 5; k++) {
        int depth = k >= 3;
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = depth ? ds : rgba, .extent = {W, W, 1}, .mipLevels = 1,
                                .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
                                .usage = (depth ? VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT : VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT) | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT};
        CHECK(vkCreateImage(dev, &ii, NULL, &im[k]));
        VkImageViewCreateInfo vi = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = im[k], .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = depth ? ds : rgba,
                                    .subresourceRange = {depth ? VK_IMAGE_ASPECT_DEPTH_BIT | VK_IMAGE_ASPECT_STENCIL_BIT : VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
        CHECK(vkCreateImageView(dev, &vi, NULL, &view[k]));
    }
    // One buffer for everything read back: three images' pixels, stencil,
    // texels, BC blocks; and one for the fans' indices.
    enum { PIXELS = 0, STENCIL = 3 * W * W * 4, TEXELS = STENCIL + W * W, BC = TEXELS + 1024, BC_BACK = BC + 32, SIZE = BC_BACK + 32 };
    VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = SIZE,
                             .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_STORAGE_TEXEL_BUFFER_BIT};
    VkBuffer buf, ibuf;
    CHECK(vkCreateBuffer(dev, &bi, NULL, &buf));
    VkBufferCreateInfo ibi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = 256, .usage = VK_BUFFER_USAGE_INDEX_BUFFER_BIT};
    CHECK(vkCreateBuffer(dev, &ibi, NULL, &ibuf));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(dev, buf, &req);
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size + 4096, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    CHECK(vkBindBufferMemory(dev, buf, mem, 0));
    CHECK(vkBindBufferMemory(dev, ibuf, mem, (req.size + 255) / 256 * 256));
    uint8_t *px;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&px));
    memset(px, 0, SIZE);
    uint16_t *indices = (uint16_t *)(px + (req.size + 255) / 256 * 256);
    for (int k = 0; k < 4; k++)
        indices[k] = (uint16_t)k;
    // (BC1: four 8-byte blocks, made up.)
    for (int k = 0; k < 32; k++)
        px[BC + k] = (uint8_t)(k * 37 + 11);

    // A render pass of a color and a depth and stencil attachment (both
    // cleared), and one of a color alone (the render area's).
    VkAttachmentDescription att[2] = {
        {0, rgba, VK_SAMPLE_COUNT_1_BIT, VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL},
        {0, ds, VK_SAMPLE_COUNT_1_BIT, VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL},
    };
    VkAttachmentReference color = {0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL}, depth = {1, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub = {0, VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1, .pColorAttachments = &color, .pDepthStencilAttachment = &depth};
    VkRenderPassCreateInfo rpi = {VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 2, .pAttachments = att, .subpassCount = 1, .pSubpasses = &sub};
    VkRenderPass pass, plain;
    CHECK(vkCreateRenderPass(dev, &rpi, NULL, &pass));
    sub.pDepthStencilAttachment = NULL;
    rpi.attachmentCount = 1;
    CHECK(vkCreateRenderPass(dev, &rpi, NULL, &plain));
    VkFramebuffer fb[3];
    for (int k = 0; k < 3; k++) {
        VkImageView v[2] = {view[k], view[k ? 2 + k : 0]};
        VkFramebufferCreateInfo fbi = {VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = k ? pass : plain, .attachmentCount = k ? 2 : 1, .pAttachments = v,
                                       .width = W, .height = W, .layers = 1};
        CHECK(vkCreateFramebuffer(dev, &fbi, NULL, &fb[k]));
    }

    // The fan pipeline: stencil always REPLACEd, its write mask dynamic.
    VkShaderModule shaders[3];
    VkShaderModuleCreateInfo si[3] = {{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof fan_vert_spv, .pCode = fan_vert_spv},
                                      {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof fan_frag_spv, .pCode = fan_frag_spv},
                                      {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof texel_write_spv, .pCode = texel_write_spv}};
    for (int k = 0; k < 3; k++)
        CHECK(vkCreateShaderModule(dev, &si[k], NULL, &shaders[k]));
    VkPipelineLayoutCreateInfo pli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    VkPipelineLayout layout;
    CHECK(vkCreatePipelineLayout(dev, &pli, NULL, &layout));
    VkPipelineShaderStageCreateInfo stages[2] = {
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = shaders[0], .pName = "main"},
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = shaders[1], .pName = "main"},
    };
    VkPipelineVertexInputStateCreateInfo vis = {VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
    VkPipelineInputAssemblyStateCreateInfo ias = {VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_FAN};
    VkViewport viewport = {0, 0, W, W, 0, 1};
    VkRect2D scissor = {{0, 0}, {W, W}};
    VkPipelineViewportStateCreateInfo vps = {VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, .viewportCount = 1, .pViewports = &viewport, .scissorCount = 1, .pScissors = &scissor};
    VkPipelineRasterizationStateCreateInfo rs = {VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE, .lineWidth = 1};
    VkPipelineMultisampleStateCreateInfo ms = {VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT};
    VkStencilOpState replace = {VK_STENCIL_OP_KEEP, VK_STENCIL_OP_REPLACE, VK_STENCIL_OP_KEEP, VK_COMPARE_OP_ALWAYS, 0xff, 0xff, 0xff};
    VkPipelineDepthStencilStateCreateInfo dss = {VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO, .depthTestEnable = VK_TRUE, .depthWriteEnable = VK_TRUE,
                                                 .depthCompareOp = VK_COMPARE_OP_ALWAYS, .stencilTestEnable = VK_TRUE, .front = replace, .back = replace};
    VkPipelineColorBlendAttachmentState cba = {.colorWriteMask = 0xf};
    VkPipelineColorBlendStateCreateInfo cbs = {VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, .attachmentCount = 1, .pAttachments = &cba};
    VkDynamicState dyn = VK_DYNAMIC_STATE_STENCIL_WRITE_MASK;
    VkPipelineDynamicStateCreateInfo ds_info = {VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, .dynamicStateCount = 1, .pDynamicStates = &dyn};
    VkGraphicsPipelineCreateInfo gpi = {VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, .stageCount = 2, .pStages = stages, .pVertexInputState = &vis,
                                        .pInputAssemblyState = &ias, .pViewportState = &vps, .pRasterizationState = &rs, .pMultisampleState = &ms,
                                        .pDepthStencilState = &dss, .pColorBlendState = &cbs, .pDynamicState = &ds_info, .layout = layout, .renderPass = pass};
    VkPipeline fan;
    CHECK(vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gpi, NULL, &fan));

    // The storage texel buffer: a view of the readback buffer's texels.
    VkBufferViewCreateInfo bvi = {VK_STRUCTURE_TYPE_BUFFER_VIEW_CREATE_INFO, .buffer = buf, .format = VK_FORMAT_R32_UINT, .offset = TEXELS, .range = 1024};
    VkBufferView texels;
    CHECK(vkCreateBufferView(dev, &bvi, NULL, &texels));
    VkDescriptorSetLayoutBinding b = {0, VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER, 1, VK_SHADER_STAGE_COMPUTE_BIT};
    VkDescriptorSetLayoutCreateInfo li = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &b};
    VkDescriptorSetLayout sl;
    CHECK(vkCreateDescriptorSetLayout(dev, &li, NULL, &sl));
    VkPipelineLayoutCreateInfo cpli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &sl};
    VkPipelineLayout clayout;
    CHECK(vkCreatePipelineLayout(dev, &cpli, NULL, &clayout));
    VkComputePipelineCreateInfo ci = {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
                                      .stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = shaders[2], .pName = "main"},
                                      .layout = clayout};
    VkPipeline writer;
    CHECK(vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &ci, NULL, &writer));
    VkDescriptorPoolSize ps = {VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER, 1};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &ps};
    VkDescriptorPool dpool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &dpool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &sl};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(dev, &dai, &set));
    VkWriteDescriptorSet wd = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_TEXEL_BUFFER,
                               .pTexelBufferView = &texels};
    vkUpdateDescriptorSets(dev, 1, &wd, 0, NULL);

    // BC1, where the device has it.
    VkImage bc = VK_NULL_HANDLE;
    if (have.textureCompressionBC) {
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_BC1_RGBA_UNORM_BLOCK, .extent = {8, 8, 1},
                                .mipLevels = 1, .arrayLayers = 1, .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
                                .usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT};
        CHECK(vkCreateImage(dev, &ii, NULL, &bc));
    }
    VkQueryPoolCreateInfo qpi = {VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, .queryType = VK_QUERY_TYPE_OCCLUSION, .queryCount = 1};
    VkQueryPool qp;
    CHECK(vkCreateQueryPool(dev, &qpi, NULL, &qp));

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    // The render area: green everywhere, then red in the left half.
    VkClearColorValue green = {.float32 = {0, 1, 0, 1}};
    VkImageSubresourceRange all = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    vkCmdClearColorImage(cmd, im[0], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &green, 1, &all);
    VkClearValue clears[2] = {{.color = {.float32 = {1, 0, 0, 1}}}, {.depthStencil = {1, 0}}};
    VkRenderPassBeginInfo rbi = {VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = plain, .framebuffer = fb[0], .renderArea = {{0, 0}, {W / 2, W}},
                                 .clearValueCount = 1, .pClearValues = clears};
    vkCmdBeginRenderPass(cmd, &rbi, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdEndRenderPass(cmd);
    // The fans, black cleared, the first in an occlusion query begun outside
    // its pass, with a stencil write mask of 0x0f.
    clears[0].color = (VkClearColorValue){.float32 = {0, 0, 0, 1}};
    vkCmdResetQueryPool(cmd, qp, 0, 1);
    vkCmdBeginQuery(cmd, qp, 0, 0);
    for (int k = 1; k <= 2; k++) {
        VkRenderPassBeginInfo fbi = {VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = pass, .framebuffer = fb[k], .renderArea = {{0, 0}, {W, W}},
                                     .clearValueCount = 2, .pClearValues = clears};
        vkCmdBeginRenderPass(cmd, &fbi, VK_SUBPASS_CONTENTS_INLINE);
        vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, fan);
        vkCmdSetStencilWriteMask(cmd, VK_STENCIL_FACE_FRONT_AND_BACK, 0x0f);
        if (k == 1) {
            vkCmdDraw(cmd, 4, 1, 0, 0);
        } else {
            vkCmdBindIndexBuffer(cmd, ibuf, 0, VK_INDEX_TYPE_UINT16);
            vkCmdDrawIndexed(cmd, 4, 1, 0, 0, 0);
        }
        vkCmdEndRenderPass(cmd);
        if (k == 1)
            vkCmdEndQuery(cmd, qp, 0);
    }
    for (int k = 0; k < 3; k++) {
        VkBufferImageCopy copy = {PIXELS + k * W * W * 4, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {W, W, 1}};
        vkCmdCopyImageToBuffer(cmd, im[k], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &copy);
    }
    VkBufferImageCopy stencil = {STENCIL, 0, 0, {VK_IMAGE_ASPECT_STENCIL_BIT, 0, 0, 1}, {0, 0, 0}, {W, W, 1}};
    vkCmdCopyImageToBuffer(cmd, im[3], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &stencil);
    // The storage texel buffer, written.
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, writer);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, clayout, 0, 1, &set, 0, NULL);
    vkCmdDispatch(cmd, 4, 1, 1);
    // BC1 in and out (rows of two blocks: 16 bytes, not WebGPU's 256).
    if (bc) {
        VkBufferImageCopy in = {BC, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {8, 8, 1}}, out = in;
        out.bufferOffset = BC_BACK;
        vkCmdCopyBufferToImage(cmd, buf, bc, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &in);
        vkCmdCopyImageToBuffer(cmd, bc, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &out);
    }
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));

    int area = 0, fans = 0, masked = 0, written = 0;
    for (int y = 0; y < W; y++)
        for (int x = 0; x < W; x++) {
            const uint8_t *p = px + PIXELS + (y * W + x) * 4;
            area += x < W / 2 ? p[0] == 255 && p[1] == 0 : p[0] == 0 && p[1] == 255;
            for (int k = 1; k <= 2; k++) {
                const uint8_t *f = px + PIXELS + k * W * W * 4 + (y * W + x) * 4;
                fans += f[0] == 0 && f[1] == 0 && f[2] == 255;
            }
            masked += px[STENCIL + y * W + x] == 0x0f;
        }
    for (uint32_t k = 0; k < 256; k++) {
        uint32_t v;
        memcpy(&v, px + TEXELS + 4 * k, 4);
        written += v == 3 * k;
    }
    uint64_t samples = 0;
    CHECK(vkGetQueryPoolResults(dev, qp, 0, 1, sizeof samples, &samples, 8, VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT));
    int blocks = !bc || memcmp(px + BC, px + BC_BACK, 32) == 0;
    printf("render area %d of %d right, fans %d of %d, stencil masked %d of %d, query %s, texels written %d of 256, BC %s, features %s\n", area, W * W, fans,
           2 * W * W, masked, W * W, samples >= W * W ? "right" : "wrong", written, blocks ? "right" : "wrong", features ? "right" : "wrong");

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyQueryPool(dev, qp, NULL);
    if (bc)
        vkDestroyImage(dev, bc, NULL);
    vkDestroyDescriptorPool(dev, dpool, NULL);
    vkDestroyPipeline(dev, writer, NULL);
    vkDestroyPipelineLayout(dev, clayout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl, NULL);
    vkDestroyBufferView(dev, texels, NULL);
    vkDestroyPipeline(dev, fan, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    for (int k = 0; k < 3; k++) {
        vkDestroyShaderModule(dev, shaders[k], NULL);
        vkDestroyFramebuffer(dev, fb[k], NULL);
    }
    vkDestroyRenderPass(dev, pass, NULL);
    vkDestroyRenderPass(dev, plain, NULL);
    for (int k = 0; k < 5; k++) {
        vkDestroyImageView(dev, view[k], NULL);
        vkDestroyImage(dev, im[k], NULL);
    }
    vkDestroyBuffer(dev, buf, NULL);
    vkDestroyBuffer(dev, ibuf, NULL);
    vkUnmapMemory(dev, mem);
    vkFreeMemory(dev, mem, NULL);
    return 0;
}

// Drawing into a 64 × 64 RGBA8 image with a depth buffer, read back: a
// green square over everything (indexed), then three triangles in its
// corners: a red one in front of it, a blue one behind it, and a yellow one
// facing away (culled). The vertex shader (vulkan_draw.vert) moves
// positions by a uniform; the fragment shader (vulkan_draw.frag) tints by
// push constants. The viewport is set before the render pass begins, the
// scissor is the pipeline's own.
static int draw(VkDevice dev, VkQueue queue) {
    enum { W = 64 };
    VkFormat formats[2] = {VK_FORMAT_R8G8B8A8_UNORM, VK_FORMAT_D32_SFLOAT};
    VkImageUsageFlags usage[2] = {VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT, VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT};
    VkImageAspectFlags aspect[2] = {VK_IMAGE_ASPECT_COLOR_BIT, VK_IMAGE_ASPECT_DEPTH_BIT};
    VkImage im[2];
    VkDeviceMemory imem[2];
    VkImageView view[2];
    for (int k = 0; k < 2; k++) {
        VkImageCreateInfo ii = {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .imageType = VK_IMAGE_TYPE_2D, .format = formats[k], .extent = {W, W, 1}, .mipLevels = 1, .arrayLayers = 1,
                                .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL, .usage = usage[k]};
        CHECK(vkCreateImage(dev, &ii, NULL, &im[k]));
        VkMemoryRequirements req;
        vkGetImageMemoryRequirements(dev, im[k], &req);
        VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size, .memoryTypeIndex = 0};
        CHECK(vkAllocateMemory(dev, &mi, NULL, &imem[k]));
        CHECK(vkBindImageMemory(dev, im[k], imem[k], 0));
        VkImageViewCreateInfo vi = {VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = im[k], .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = formats[k],
                                    .subresourceRange = {aspect[k], 0, 1, 0, 1}};
        CHECK(vkCreateImageView(dev, &vi, NULL, &view[k]));
    }

    // One memory: vertices (position, color), indices, the uniform, and
    // the image read back.
    float offset[4] = {0.25f, -0.125f, 0, 0};
    struct vertex {
        float x, y, z, r, g, b;
    } v[13] = {
        // The square (Vulkan's y is down: counter-clockwise on screen is
        // front-facing).
        {-1, -1, 0.5f, 0, 1, 0},
        {-1, 1, 0.5f, 0, 1, 0},
        {1, 1, 0.5f, 0, 1, 0},
        {1, -1, 0.5f, 0, 1, 0},
        // Top left, in front; bottom right, behind; bottom left, facing away.
        {-1, -1, 0.25f, 1, 0, 0},
        {-1, 0, 0.25f, 1, 0, 0},
        {0, -1, 0.25f, 1, 0, 0},
        {1, 1, 0.75f, 0, 0, 1},
        {1, 0, 0.75f, 0, 0, 1},
        {0, 1, 0.75f, 0, 0, 1},
        {-1, 1, 0.1f, 1, 1, 0},
        {-1, 0, 0.1f, 1, 1, 0},
        {0, 1, 0.1f, 1, 1, 0},
    };
    for (int k = 0; k < 13; k++)
        v[k].x -= offset[0], v[k].y -= offset[1];
    uint16_t indices[6] = {0, 1, 2, 0, 2, 3};
    VkDeviceSize size[4] = {sizeof v, sizeof indices, sizeof offset, W * W * 4}, at[4], total = 0;
    VkBufferUsageFlags busage[4] = {VK_BUFFER_USAGE_VERTEX_BUFFER_BIT, VK_BUFFER_USAGE_INDEX_BUFFER_BIT, VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT, VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf[4];
    for (int k = 0; k < 4; k++) {
        VkBufferCreateInfo bi = {VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = size[k], .usage = busage[k]};
        CHECK(vkCreateBuffer(dev, &bi, NULL, &buf[k]));
        VkMemoryRequirements req;
        vkGetBufferMemoryRequirements(dev, buf[k], &req);
        at[k] = (total + req.alignment - 1) / req.alignment * req.alignment;
        total = at[k] + req.size;
    }
    VkMemoryAllocateInfo mi = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = total, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    CHECK(vkAllocateMemory(dev, &mi, NULL, &mem));
    for (int k = 0; k < 4; k++)
        CHECK(vkBindBufferMemory(dev, buf[k], mem, at[k]));
    uint8_t *map;
    CHECK(vkMapMemory(dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&map));
    memcpy(map + at[0], v, sizeof v);
    memcpy(map + at[1], indices, sizeof indices);
    memcpy(map + at[2], offset, sizeof offset);
    uint8_t *pixels = map + at[3];

    VkAttachmentDescription att[2] = {
        {0, formats[0], VK_SAMPLE_COUNT_1_BIT, VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_STORE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL},
        {0, formats[1], VK_SAMPLE_COUNT_1_BIT, VK_ATTACHMENT_LOAD_OP_CLEAR, VK_ATTACHMENT_STORE_OP_DONT_CARE, VK_ATTACHMENT_LOAD_OP_DONT_CARE, VK_ATTACHMENT_STORE_OP_DONT_CARE,
         VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL},
    };
    VkAttachmentReference color_ref = {0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL}, depth_ref = {1, VK_IMAGE_LAYOUT_DEPTH_STENCIL_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub = {0, VK_PIPELINE_BIND_POINT_GRAPHICS, .colorAttachmentCount = 1, .pColorAttachments = &color_ref, .pDepthStencilAttachment = &depth_ref};
    VkRenderPassCreateInfo rpi = {VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO, .attachmentCount = 2, .pAttachments = att, .subpassCount = 1, .pSubpasses = &sub};
    VkRenderPass pass;
    CHECK(vkCreateRenderPass(dev, &rpi, NULL, &pass));
    VkFramebufferCreateInfo fbi = {VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = pass, .attachmentCount = 2, .pAttachments = view, .width = W, .height = W, .layers = 1};
    VkFramebuffer fb;
    CHECK(vkCreateFramebuffer(dev, &fbi, NULL, &fb));

    VkShaderModule shaders[2];
    VkShaderModuleCreateInfo si[2] = {{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof vert_spv, .pCode = vert_spv},
                                      {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = sizeof frag_spv, .pCode = frag_spv}};
    for (int k = 0; k < 2; k++)
        CHECK(vkCreateShaderModule(dev, &si[k], NULL, &shaders[k]));
    VkDescriptorSetLayoutBinding b = {0, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1, VK_SHADER_STAGE_VERTEX_BIT};
    VkDescriptorSetLayoutCreateInfo li = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, .bindingCount = 1, .pBindings = &b};
    VkDescriptorSetLayout sl;
    CHECK(vkCreateDescriptorSetLayout(dev, &li, NULL, &sl));
    VkPushConstantRange pr = {VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16};
    VkPipelineLayoutCreateInfo pli = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, .setLayoutCount = 1, .pSetLayouts = &sl, .pushConstantRangeCount = 1, .pPushConstantRanges = &pr};
    VkPipelineLayout layout;
    CHECK(vkCreatePipelineLayout(dev, &pli, NULL, &layout));
    // (The fragment shader's scale, specialized: 1.)
    float one = 1;
    VkSpecializationMapEntry se = {7, 0, 4};
    VkSpecializationInfo spec = {1, &se, 4, &one};
    VkPipelineShaderStageCreateInfo stages[2] = {
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = shaders[0], .pName = "main"},
        {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = shaders[1], .pName = "main", .pSpecializationInfo = &spec},
    };
    VkVertexInputBindingDescription vb = {0, sizeof(struct vertex), VK_VERTEX_INPUT_RATE_VERTEX};
    VkVertexInputAttributeDescription va[2] = {{0, 0, VK_FORMAT_R32G32B32_SFLOAT, 0}, {1, 0, VK_FORMAT_R32G32B32_SFLOAT, 12}};
    VkPipelineVertexInputStateCreateInfo vis = {VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, .vertexBindingDescriptionCount = 1, .pVertexBindingDescriptions = &vb,
                                                .vertexAttributeDescriptionCount = 2, .pVertexAttributeDescriptions = va};
    VkPipelineInputAssemblyStateCreateInfo ias = {VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkRect2D scissor = {{0, 0}, {W, W}};
    VkPipelineViewportStateCreateInfo vps = {VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, .viewportCount = 1, .scissorCount = 1, .pScissors = &scissor};
    VkPipelineRasterizationStateCreateInfo rs = {VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_BACK_BIT,
                                                 .frontFace = VK_FRONT_FACE_COUNTER_CLOCKWISE, .lineWidth = 1};
    VkPipelineMultisampleStateCreateInfo ms = {VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT};
    VkPipelineDepthStencilStateCreateInfo dss = {VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO, .depthTestEnable = VK_TRUE, .depthWriteEnable = VK_TRUE,
                                                 .depthCompareOp = VK_COMPARE_OP_LESS};
    VkPipelineColorBlendAttachmentState cba = {.colorWriteMask = 0xf};
    VkPipelineColorBlendStateCreateInfo cbs = {VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, .attachmentCount = 1, .pAttachments = &cba};
    VkDynamicState dyn = VK_DYNAMIC_STATE_VIEWPORT;
    VkPipelineDynamicStateCreateInfo ds = {VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, .dynamicStateCount = 1, .pDynamicStates = &dyn};
    VkGraphicsPipelineCreateInfo gpi = {VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
                                        .stageCount = 2,
                                        .pStages = stages,
                                        .pVertexInputState = &vis,
                                        .pInputAssemblyState = &ias,
                                        .pViewportState = &vps,
                                        .pRasterizationState = &rs,
                                        .pMultisampleState = &ms,
                                        .pDepthStencilState = &dss,
                                        .pColorBlendState = &cbs,
                                        .pDynamicState = &ds,
                                        .layout = layout,
                                        .renderPass = pass};
    VkPipeline pipeline;
    CHECK(vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gpi, NULL, &pipeline));

    VkDescriptorPoolSize ps = {VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1};
    VkDescriptorPoolCreateInfo dpi = {VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO, .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &ps};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(dev, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo dai = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO, .descriptorPool = pool, .descriptorSetCount = 1, .pSetLayouts = &sl};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(dev, &dai, &set));
    VkDescriptorBufferInfo bu = {buf[2], 0, sizeof offset};
    VkWriteDescriptorSet w = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set, .dstBinding = 0, .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, .pBufferInfo = &bu};
    vkUpdateDescriptorSets(dev, 1, &w, 0, NULL);

    VkCommandPoolCreateInfo cpi = {VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = 0};
    VkCommandPool cpool;
    CHECK(vkCreateCommandPool(dev, &cpi, NULL, &cpool));
    VkCommandBufferAllocateInfo cai = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = cpool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cmd));
    // (Queries: occlusion around the square and around nothing; timestamps
    // around the pass.)
    VkQueryPool qp[2];
    VkQueryPoolCreateInfo qi[2] = {{VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, .queryType = VK_QUERY_TYPE_OCCLUSION, .queryCount = 2},
                                   {VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO, .queryType = VK_QUERY_TYPE_TIMESTAMP, .queryCount = 2}};
    for (int k = 0; k < 2; k++)
        CHECK(vkCreateQueryPool(dev, &qi[k], NULL, &qp[k]));
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    VkViewport viewport = {0, 0, W, W, 0, 1};
    vkCmdSetViewport(cmd, 0, 1, &viewport);
    for (int k = 0; k < 2; k++)
        vkCmdResetQueryPool(cmd, qp[k], 0, 2);
    vkCmdWriteTimestamp(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, qp[1], 0);
    VkClearValue clears[2] = {{.color = {.float32 = {0, 0, 0, 1}}}, {.depthStencil = {1, 0}}};
    VkRenderPassBeginInfo rbi = {VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = pass, .framebuffer = fb, .renderArea = {{0, 0}, {W, W}}, .clearValueCount = 2, .pClearValues = clears};
    vkCmdBeginRenderPass(cmd, &rbi, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, layout, 0, 1, &set, 0, NULL);
    float tint[4] = {1, 0.5f, 1, 1};
    vkCmdPushConstants(cmd, layout, VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16, tint);
    VkDeviceSize zero = 0;
    vkCmdBindVertexBuffers(cmd, 0, 1, &buf[0], &zero);
    vkCmdBindIndexBuffer(cmd, buf[1], 0, VK_INDEX_TYPE_UINT16);
    vkCmdBeginQuery(cmd, qp[0], 0, 0);
    vkCmdDrawIndexed(cmd, 6, 1, 0, 0, 0);
    vkCmdEndQuery(cmd, qp[0], 0);
    vkCmdBeginQuery(cmd, qp[0], 1, 0);
    vkCmdEndQuery(cmd, qp[0], 1);
    vkCmdDraw(cmd, 9, 1, 4, 0);
    vkCmdEndRenderPass(cmd);
    vkCmdWriteTimestamp(cmd, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, qp[1], 1);
    VkBufferImageCopy whole = {0, 0, 0, {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, {0, 0, 0}, {W, W, 1}};
    vkCmdCopyImageToBuffer(cmd, im[0], VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf[3], 1, &whole);
    CHECK(vkEndCommandBuffer(cmd));
    VkSubmitInfo submit = {VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    CHECK(vkQueueSubmit(queue, 1, &submit, VK_NULL_HANDLE));
    CHECK(vkQueueWaitIdle(queue));

    // Pixels well inside each corner, and the middle.
    struct {
        int x, y;
        uint8_t rgba[4];
    } want[5] = {
        {6, 6, {255, 0, 0, 255}}, {58, 58, {0, 128, 0, 255}}, {6, 58, {0, 128, 0, 255}}, {58, 6, {0, 128, 0, 255}}, {40, 24, {0, 128, 0, 255}},
    };
    int wrong = 0;
    for (int k = 0; k < 5; k++) {
        uint8_t *p = pixels + (want[k].y * W + want[k].x) * 4;
        int bad = 0;
        for (int c = 0; c < 4; c++)
            bad |= p[c] < want[k].rgba[c] - 1 || p[c] > want[k].rgba[c] + 1;
        if (bad)
            printf("pixel (%d, %d): %d %d %d %d\n", want[k].x, want[k].y, p[0], p[1], p[2], p[3]);
        wrong += bad;
    }
    printf("draw: 5 pixels looked at, %d wrong\n", wrong);
    uint64_t occluded[2], stamps[2];
    CHECK(vkGetQueryPoolResults(dev, qp[0], 0, 2, sizeof occluded, occluded, 8, VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT));
    CHECK(vkGetQueryPoolResults(dev, qp[1], 0, 2, sizeof stamps, stamps, 8, VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT));
    printf("queries: occlusion %s, timestamps %s\n", occluded[0] > 0 && occluded[1] == 0 ? "right" : "wrong", stamps[1] > stamps[0] ? "in order" : "out of order");
    for (int k = 0; k < 2; k++)
        vkDestroyQueryPool(dev, qp[k], NULL);

    vkFreeCommandBuffers(dev, cpool, 1, &cmd);
    vkDestroyCommandPool(dev, cpool, NULL);
    vkDestroyDescriptorPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    vkDestroyDescriptorSetLayout(dev, sl, NULL);
    for (int k = 0; k < 2; k++)
        vkDestroyShaderModule(dev, shaders[k], NULL);
    vkDestroyFramebuffer(dev, fb, NULL);
    vkDestroyRenderPass(dev, pass, NULL);
    for (int k = 0; k < 2; k++) {
        vkDestroyImageView(dev, view[k], NULL);
        vkDestroyImage(dev, im[k], NULL);
        vkFreeMemory(dev, imem[k], NULL);
    }
    for (int k = 0; k < 4; k++)
        vkDestroyBuffer(dev, buf[k], NULL);
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
    // (extra, specialized: 5.)
    uint32_t extra = 5;
    VkSpecializationMapEntry se = {5, 0, 4};
    VkSpecializationInfo spec = {1, &se, 4, &extra};
    VkComputePipelineCreateInfo ci = {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
                                      .stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = shader, .pName = "main",
                                                .pSpecializationInfo = &spec},
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
        wrong += out[i] != i * 7 * 3 + 11 + 5 || copy[i] != out[i];
    printf("compute: %d of %d wrong\n", wrong, N);
    printf("fill and update: %s\n", filled[0] == 0x5a5a5a5a && filled[2] == 0xc0ffee && filled[3] == 0xbeef && filled[15] == 0x5a5a5a5a ? "right" : "wrong");
    printf("the program's own write kept: %s\n", unused[0] == 12345 ? "yes" : "no");
    if (images(dev, queue) || blits(dev, queue) || clears(dev, pd, queue) || draw(dev, queue) || subpasses(dev, queue) || texels(dev, queue) || arrays(dev, queue) || gaps(pd, dev, queue) || layouts(dev, queue))
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
