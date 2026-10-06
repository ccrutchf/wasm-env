// tests/arm64/vulkan.c's fragment shader: colors tinted by push constants,
// and scaled by a specialization constant (0 unless specialized).
#version 450
layout(location = 0) in vec3 v_color;
layout(location = 0) out vec4 o;
layout(push_constant) uniform Tint {
    vec4 tint;
} p;
layout(constant_id = 7) const float scale = 0.0;

void main() {
    o = vec4(v_color * p.tint.rgb * scale, 1.0);
}
