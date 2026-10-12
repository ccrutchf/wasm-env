// tests/arm64/vulkan.c's multiview: each view its own color (view 0 red,
// view 1 green).
#version 450
#extension GL_EXT_multiview : require
layout(location = 0) out vec4 color;
void main() {
    color = gl_ViewIndex == 0 ? vec4(1.0, 0.0, 0.0, 1.0) : vec4(0.0, 1.0, 0.0, 1.0);
}
