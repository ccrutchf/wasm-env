// tests/arm64/vulkan.c's second subpass: a triangle covering the target.
#version 450

void main() {
    gl_Position = vec4(float(gl_VertexIndex & 1) * 4.0 - 1.0, float(gl_VertexIndex >> 1) * 4.0 - 1.0, 0.0, 1.0);
}
