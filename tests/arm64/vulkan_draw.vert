// tests/arm64/vulkan.c's vertex shader: positions moved by a uniform.
#version 450
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 color;
layout(location = 0) out vec3 v_color;
layout(set = 0, binding = 0) uniform Offset {
    vec4 offset;
} u;

void main() {
    gl_Position = vec4(position.xy + u.offset.xy, position.z, 1.0);
    v_color = color;
}
