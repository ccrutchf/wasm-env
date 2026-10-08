// tests/arm64/vulkan.c's triangle fans: a quad over the whole target, its
// corners in a fan's order, at depth 0.5.
#version 450
const vec2 corners[4] = vec2[](vec2(-1, -1), vec2(1, -1), vec2(1, 1), vec2(-1, 1));

void main() {
    gl_Position = vec4(corners[gl_VertexIndex & 3], 0.5, 1);
}
