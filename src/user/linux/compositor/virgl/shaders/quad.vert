#version 310 es
// The one vertex shader. A vertex is its place in pixels of the target and
// a texture coordinate; `to_clip` is (2/width, 2/height, -1, -1), which takes
// pixels to clip space with nothing flipped. Both are handed on in one
// vector -- the coordinate in xy, the place in zw, because every shape here
// is cut in pixels -- and in one on purpose: a linker packs two outputs to
// suit whichever of them a fragment shader reads, and this one vertex
// shader serves them all.
precision highp float;
layout(location = 0) in vec2 pos;
layout(location = 1) in vec2 texcoord;
uniform vec4 to_clip;
layout(location = 0) out vec4 v;
void main() {
    gl_Position = vec4(pos * to_clip.xy + to_clip.zw, 0.0, 1.0);
    v = vec4(texcoord, pos);
}
