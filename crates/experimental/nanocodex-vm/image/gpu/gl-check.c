/* Real GPU readback AND X11 presentation. Run through nanocodex-gpu-gl. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <GL/gl.h>
#include <GL/glx.h>

#define CHECK(condition, message) do { \
    if (!(condition)) { fprintf(stderr, "%s\n", message); result = 1; goto done; } \
} while (0)

int main(void) {
    int result = 0;
    Display *display = XOpenDisplay(NULL);
    if (!display) {
        fprintf(stderr, "No X11 desktop; set DISPLAY and XAUTHORITY\n");
        return 1;
    }
    XVisualInfo *visual = NULL;
    GLXContext context = NULL;
    Window window = None;
    Colormap colormap = None;
    int attributes[] = {GLX_RGBA, GLX_DOUBLEBUFFER, GLX_RED_SIZE, 8,
                        GLX_GREEN_SIZE, 8, GLX_BLUE_SIZE, 8, None};
    visual = glXChooseVisual(display, DefaultScreen(display), attributes);
    CHECK(visual, "No RGB GLX visual");
    colormap = XCreateColormap(display, RootWindow(display, visual->screen),
                              visual->visual, AllocNone);
    XSetWindowAttributes wa = {0};
    wa.colormap = colormap;
    wa.override_redirect = True;
    window = XCreateWindow(display, RootWindow(display, visual->screen),
                           0, 0, 256, 256, 0, visual->depth, InputOutput,
                           visual->visual, CWColormap | CWOverrideRedirect, &wa);
    XStoreName(display, window, "Nanocodex GPU graphics check");
    XMapWindow(display, window);
    XSync(display, False);
    context = glXCreateContext(display, visual, NULL, True);
    CHECK(context, "Cannot create GLX context");
    CHECK(glXMakeCurrent(display, window, context), "Cannot bind GLX context");
    const char *renderer = (const char *)glGetString(GL_RENDERER);
    CHECK(renderer && strstr(renderer, "zink") &&
          !strstr(renderer, "llvmpipe") && !strstr(renderer, "lavapipe"),
          "Hardware Zink renderer required");
    printf("renderer=%s\nversion=%s\n", renderer, glGetString(GL_VERSION));

    /* Odd widths exercise padded GPU row strides and X11 clipping. */
    const unsigned sizes[][2] = {{256, 256}, {127, 91}, {381, 219}, {32, 32}};
    for (unsigned i = 0; i < sizeof(sizes) / sizeof(sizes[0]); i++) {
        unsigned width = sizes[i][0], height = sizes[i][1];
        XResizeWindow(display, window, width, height);
        XSync(display, False);
        glViewport(0, 0, width, height);
        glClearColor(1, 0, 0, 1);
        glClear(GL_COLOR_BUFFER_BIT);
        glBegin(GL_TRIANGLES);
        glColor3f(0, 1, 0);
        glVertex2f(-1, -1);
        glVertex2f(1, -1);
        glVertex2f(0, 1);
        glEnd();
        glFinish();
        unsigned char pixel[4] = {0};
        glReadPixels(width / 2, height / 2, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
        CHECK(glGetError() == GL_NO_ERROR, "OpenGL rendering/readback error");
        CHECK(pixel[0] == 0 && pixel[1] == 255 && pixel[2] == 0,
              "GPU readback did not contain the green triangle");
        glXSwapBuffers(display, window);
        XSync(display, False);
        XImage *image = XGetImage(display, window, 0, 0, width, height, AllPlanes, ZPixmap);
        CHECK(image, "Cannot read presented X11 window");
        unsigned long center = XGetPixel(image, width / 2, height / 2);
        unsigned long corner = XGetPixel(image, width - 2, height / 2);
        int correct = (center & image->green_mask) == image->green_mask &&
                      !(center & (image->red_mask | image->blue_mask)) &&
                      (corner & image->red_mask) == image->red_mask &&
                      !(corner & (image->green_mask | image->blue_mask));
        XDestroyImage(image);
        CHECK(correct, "Presented pixels differ from the GPU-rendered triangle/background");
        printf("size=%ux%u readback=pass presentation=pass\n", width, height);
    }
done:
    if (context) {
        glXMakeCurrent(display, None, NULL);
        glXDestroyContext(display, context);
    }
    if (window) XDestroyWindow(display, window);
    if (colormap) XFreeColormap(display, colormap);
    if (visual) XFree(visual);
    XCloseDisplay(display);
    return result;
}
