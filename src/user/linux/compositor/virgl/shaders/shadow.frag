#version 310 es
// A drop shadow: `shadow.glsl`'s falloff over the shadow's own box u[1],
// in the premultiplied colour u[0]. u[2] is (inset, range, power, 0), the
// inset being the range and the window's rounding together.
precision highp float;
layout(location = 0) in vec4 v;
#define v_texcoord v.xy
#define v_pos v.zw
uniform vec4 u[3];
layout(location = 0) out vec4 color;
float faded(float share, float power) {
    return pow(clamp(share, 0.0, 1.0), power);
}
void main() {
    vec2 p = v_pos - u[1].xy;
    vec2 size = u[1].zw;
    float inset = u[2].x;
    float range = u[2].y;
    float power = u[2].z;
    // How far past the rounding's centre the pixel is on each axis: both
    // positive only in a corner.
    vec2 d = max(max(inset - p, p - (size - inset)), 0.0);
    float alpha = 1.0;
    if (d.x > 0.0 && d.y > 0.0) {
        float distance = length(d);
        if (distance > inset)
            alpha = 0.0;
        else if (distance > inset - range)
            alpha = faded((inset - distance) / range, power);
    } else {
        float smallest = min(min(p.y, size.y - p.y), min(p.x, size.x - p.x));
        if (smallest < range)
            alpha = faded(smallest / range, power);
    }
    color = u[0] * alpha;
}
