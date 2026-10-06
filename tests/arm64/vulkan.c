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
    VkCommandBufferBeginInfo cbi = {VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    CHECK(vkBeginCommandBuffer(cmd, &cbi));
    VkViewport viewport = {0, 0, W, W, 0, 1};
    vkCmdSetViewport(cmd, 0, 1, &viewport);
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
    vkCmdDrawIndexed(cmd, 6, 1, 0, 0, 0);
    vkCmdDraw(cmd, 9, 1, 4, 0);
    vkCmdEndRenderPass(cmd);
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
    if (images(dev, queue) || blits(dev, queue) || draw(dev, queue))
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
