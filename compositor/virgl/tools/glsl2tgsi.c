// Compile a GLSL vertex and fragment shader on Mesa's virgl driver and draw
// once with them, so that VIRGL_DEBUG=tgsi makes the driver print the TGSI
// it sends to virglrenderer. tools/regenerate.sh is what runs this; it says
// why.
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl31.h>
#include <stdio.h>
#include <stdlib.h>

static char *slurp(const char *path) {
    FILE *file = fopen(path, "rb");
    if (!file) {
        perror(path);
        exit(2);
    }
    fseek(file, 0, SEEK_END);
    long size = ftell(file);
    fseek(file, 0, SEEK_SET);
    char *text = malloc((size_t)size + 1);
    if (!text || fread(text, 1, (size_t)size, file) != (size_t)size) {
        fprintf(stderr, "%s: short read\n", path);
        exit(2);
    }
    text[size] = 0;
    fclose(file);
    return text;
}

static GLuint compile(GLenum kind, const char *path) {
    const char *source = slurp(path);
    GLuint shader = glCreateShader(kind);
    glShaderSource(shader, 1, &source, NULL);
    glCompileShader(shader);
    GLint ok = 0;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[8192];
        glGetShaderInfoLog(shader, sizeof log, NULL, log);
        fprintf(stderr, "%s: %s\n", path, log);
        exit(3);
    }
    return shader;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: glsl2tgsi <vertex> <fragment>\n");
        return 1;
    }
    EGLDisplay display =
        eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
    if (!eglInitialize(display, NULL, NULL)) {
        fprintf(stderr, "no EGL display\n");
        return 4;
    }
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLint context_attributes[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, 1,
                                   EGL_NONE};
    EGLContext context =
        eglCreateContext(display, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, context_attributes);
    if (context == EGL_NO_CONTEXT ||
        !eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, context)) {
        fprintf(stderr, "no GLES 3.1 context: 0x%x\n", eglGetError());
        return 4;
    }
    fprintf(stderr, "renderer: %s\n", glGetString(GL_RENDERER));

    GLuint program = glCreateProgram();
    glAttachShader(program, compile(GL_VERTEX_SHADER, argv[1]));
    glAttachShader(program, compile(GL_FRAGMENT_SHADER, argv[2]));
    glLinkProgram(program);
    GLint ok = 0;
    glGetProgramiv(program, GL_LINK_STATUS, &ok);
    if (!ok) {
        char log[8192];
        glGetProgramInfoLog(program, sizeof log, NULL, log);
        fprintf(stderr, "link: %s\n", log);
        return 5;
    }

    // Somewhere to draw and something to read: the driver sends a shader
    // when a draw first needs it.
    GLuint textures[2], framebuffer, buffer;
    glGenTextures(2, textures);
    for (int i = 0; i < 2; i++) {
        glBindTexture(GL_TEXTURE_2D, textures[i]);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 64, 64, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    }
    glGenFramebuffers(1, &framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, textures[0], 0);
    glViewport(0, 0, 64, 64);
    glUseProgram(program);
    glBindTexture(GL_TEXTURE_2D, textures[1]);
    float vertices[] = {0, 0, 0, 0, 64, 0, 1, 0, 0, 64, 0, 1, 64, 64, 1, 1};
    glGenBuffers(1, &buffer);
    glBindBuffer(GL_ARRAY_BUFFER, buffer);
    glBufferData(GL_ARRAY_BUFFER, sizeof vertices, vertices, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 16, (void *)0);
    glEnableVertexAttribArray(1);
    glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 16, (void *)8);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glFinish();
    return 0;
}
