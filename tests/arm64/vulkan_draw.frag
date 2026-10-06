// tests/arm64/vulkan.c's fragment shader: colors tinted by push constants.
#version 450
layout(location = 0) in vec3 v_color;
layout(location = 0) out vec4 o;
layout(push_constant) uniform Tint {
    vec4 tint;
} p;

void main() {
    o = vec4(v_color * p.tint.rgb, 1.0);
}
