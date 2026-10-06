// tests/arm64/vulkan.c's second subpass: the first subpass's color, read as
// an input attachment, inverted.
#version 450
layout(input_attachment_index = 0, set = 0, binding = 0) uniform subpassInput scene;
layout(location = 0) out vec4 o;

void main() {
    vec4 c = subpassLoad(scene);
    o = vec4(1.0 - c.rgb, 1.0);
}
