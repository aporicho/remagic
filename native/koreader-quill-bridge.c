// SPDX-License-Identifier: MIT
// A small framebuffer compatibility bridge for KOReader on reMarkable Move.
// It presents a conventional RGB565 /dev/fb0 and submits dirty rectangles to
// Quill. This is an independent implementation built only from Linux UAPI.

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/fb.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "quill.h"

#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001U
#endif

struct mxcfb_rect_compat {
    uint32_t top;
    uint32_t left;
    uint32_t width;
    uint32_t height;
};

struct mxcfb_update_compat {
    struct mxcfb_rect_compat update_region;
    uint32_t waveform_mode;
    uint32_t update_mode;
    uint32_t update_marker;
    int32_t temperature;
    uint32_t flags;
    int32_t dither_mode;
    uint32_t quant_bit;
};

#define REMAGIC_MXCFB_SEND_UPDATE _IOW('F', 0x2E, struct mxcfb_update_compat)

static pthread_mutex_t bridge_lock = PTHREAD_MUTEX_INITIALIZER;
static int framebuffer_fd = -1;
static dev_t framebuffer_dev;
static ino_t framebuffer_ino;
static uint16_t *rgb565;
static uint32_t *quill_pixels;
static int panel_width;
static int panel_height;
static int quill_row_stride;

static int (*real_open_fn)(const char *, int, ...);
static int (*real_openat_fn)(int, const char *, int, ...);
static int (*real_ioctl_fn)(int, unsigned long, ...);

static void resolve_symbols(void) {
    if (!real_open_fn) real_open_fn = dlsym(RTLD_NEXT, "open");
    if (!real_openat_fn) real_openat_fn = dlsym(RTLD_NEXT, "openat");
    if (!real_ioctl_fn) real_ioctl_fn = dlsym(RTLD_NEXT, "ioctl");
}

static bool is_framebuffer_path(const char *path) {
    return path && (!strcmp(path, "/dev/fb0") || !strcmp(path, "/dev/graphics/fb0"));
}

static bool flags_have_mode(int flags) {
    return (flags & O_CREAT) != 0 || (flags & O_TMPFILE) == O_TMPFILE;
}

static bool is_bridge_fd(int fd) {
    struct stat status;
    return framebuffer_fd >= 0 && fstat(fd, &status) == 0 &&
           status.st_dev == framebuffer_dev && status.st_ino == framebuffer_ino;
}

static int initialize_bridge(void) {
    if (framebuffer_fd >= 0) return 0;
    if (quill_init() != 0) {
        errno = ENODEV;
        return -1;
    }
    panel_width = quill_width();
    panel_height = quill_height();
    quill_row_stride = quill_stride();
    quill_pixels = (uint32_t *)quill_buffer();
    if (panel_width <= 0 || panel_height <= 0 ||
        quill_row_stride < panel_width * 4 || !quill_pixels) {
        errno = ENODEV;
        return -1;
    }

    framebuffer_fd = (int)syscall(SYS_memfd_create, "remagic-fb0", MFD_CLOEXEC);
    if (framebuffer_fd < 0) return -1;
    size_t length = (size_t)panel_width * (size_t)panel_height * 2U;
    if (ftruncate(framebuffer_fd, (off_t)length) != 0) goto fail;
    rgb565 = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, framebuffer_fd, 0);
    if (rgb565 == MAP_FAILED) {
        rgb565 = NULL;
        goto fail;
    }
    struct stat status;
    if (fstat(framebuffer_fd, &status) != 0) goto fail;
    framebuffer_dev = status.st_dev;
    framebuffer_ino = status.st_ino;
    fprintf(stderr, "remagic-fb: RGB565 framebuffer %dx%d\n", panel_width, panel_height);
    return 0;

fail:
    {
        int saved = errno;
        if (rgb565) munmap(rgb565, length);
        rgb565 = NULL;
        close(framebuffer_fd);
        framebuffer_fd = -1;
        errno = saved;
        return -1;
    }
}

static int duplicate_framebuffer(int flags) {
    pthread_mutex_lock(&bridge_lock);
    int result = initialize_bridge();
    if (result == 0) {
        int command = (flags & O_CLOEXEC) ? F_DUPFD_CLOEXEC : F_DUPFD;
        result = fcntl(framebuffer_fd, command, 3);
    }
    pthread_mutex_unlock(&bridge_lock);
    return result;
}

static void convert_and_submit(uint32_t left, uint32_t top, uint32_t width,
                               uint32_t height, uint32_t waveform,
                               uint32_t update_mode) {
    if (left >= (uint32_t)panel_width || top >= (uint32_t)panel_height) return;
    if (!width || width > (uint32_t)panel_width - left) width = (uint32_t)panel_width - left;
    if (!height || height > (uint32_t)panel_height - top) height = (uint32_t)panel_height - top;

    for (uint32_t y = top; y < top + height; ++y) {
        const uint16_t *source = rgb565 + (size_t)y * panel_width + left;
        uint32_t *target =
            (uint32_t *)((uint8_t *)quill_pixels + (size_t)y * quill_row_stride) + left;
        for (uint32_t x = 0; x < width; ++x) {
            uint16_t pixel = source[x];
            uint32_t red = ((pixel >> 11) & 0x1fU) * 255U / 31U;
            uint32_t green = ((pixel >> 5) & 0x3fU) * 255U / 63U;
            uint32_t blue = (pixel & 0x1fU) * 255U / 31U;
            target[x] = 0xff000000U | (red << 16) | (green << 8) | blue;
        }
    }

    const bool full = update_mode == 1U;
    const int mode = full ? QUILL_MODE_COLOR4 : ((waveform == 1U || waveform == 4U) ?
                                                   QUILL_MODE_FASTEST : QUILL_MODE_COLOR3);
    quill_swap_ex((int)left, (int)top, (int)width, (int)height, mode,
                  full ? 1 : 0, QUILL_CONTENT_MONO);
    quill_process_events();
}

static void fill_var(struct fb_var_screeninfo *var) {
    memset(var, 0, sizeof(*var));
    var->xres = var->xres_virtual = (uint32_t)panel_width;
    var->yres = var->yres_virtual = (uint32_t)panel_height;
    var->bits_per_pixel = 16;
    var->red.offset = 11; var->red.length = 5;
    var->green.offset = 5; var->green.length = 6;
    var->blue.offset = 0; var->blue.length = 5;
    var->activate = FB_ACTIVATE_NOW;
    var->height = var->width = UINT32_MAX;
}

static void fill_fix(struct fb_fix_screeninfo *fix) {
    memset(fix, 0, sizeof(*fix));
    strncpy(fix->id, "remagic-quill", sizeof(fix->id) - 1);
    fix->smem_len = (uint32_t)((size_t)panel_width * panel_height * 2U);
    fix->type = FB_TYPE_PACKED_PIXELS;
    fix->visual = FB_VISUAL_TRUECOLOR;
    fix->line_length = (uint32_t)panel_width * 2U;
    fix->accel = FB_ACCEL_NONE;
}

int open(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags_have_mode(flags)) {
        va_list arguments; va_start(arguments, flags); mode = (mode_t)va_arg(arguments, int); va_end(arguments);
    }
    if (is_framebuffer_path(path)) return duplicate_framebuffer(flags);
    resolve_symbols();
    return flags_have_mode(flags) ? real_open_fn(path, flags, mode) : real_open_fn(path, flags);
}

int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags_have_mode(flags)) {
        va_list arguments; va_start(arguments, flags); mode = (mode_t)va_arg(arguments, int); va_end(arguments);
    }
    if (is_framebuffer_path(path)) return duplicate_framebuffer(flags);
    resolve_symbols();
    return flags_have_mode(flags) ? real_open_fn(path, flags, mode) : real_open_fn(path, flags);
}

int openat(int directory, const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags_have_mode(flags)) {
        va_list arguments; va_start(arguments, flags); mode = (mode_t)va_arg(arguments, int); va_end(arguments);
    }
    if (is_framebuffer_path(path)) return duplicate_framebuffer(flags);
    resolve_symbols();
    return flags_have_mode(flags) ? real_openat_fn(directory, path, flags, mode) :
                                             real_openat_fn(directory, path, flags);
}

int openat64(int directory, const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags_have_mode(flags)) {
        va_list arguments; va_start(arguments, flags); mode = (mode_t)va_arg(arguments, int); va_end(arguments);
    }
    if (is_framebuffer_path(path)) return duplicate_framebuffer(flags);
    resolve_symbols();
    return flags_have_mode(flags) ? real_openat_fn(directory, path, flags, mode) :
                                             real_openat_fn(directory, path, flags);
}

int ioctl(int fd, unsigned long request, ...) {
    void *argument = NULL;
    va_list arguments; va_start(arguments, request); argument = va_arg(arguments, void *); va_end(arguments);
    if (!is_bridge_fd(fd)) {
        resolve_symbols();
        return real_ioctl_fn(fd, request, argument);
    }
    if (!argument && request != FBIOBLANK) {
        errno = EFAULT;
        return -1;
    }
    switch (request) {
        case FBIOGET_VSCREENINFO: fill_var(argument); return 0;
        case FBIOGET_FSCREENINFO: fill_fix(argument); return 0;
        case FBIOPUT_VSCREENINFO: return 0;
        case FBIOPAN_DISPLAY: {
            struct fb_var_screeninfo *var = argument;
            if (!var) { errno = EFAULT; return -1; }
            convert_and_submit(0, 0, (uint32_t)panel_width, (uint32_t)panel_height, 0, 0);
            fill_var(var);
            return 0;
        }
        case FBIOBLANK: return 0;
        case REMAGIC_MXCFB_SEND_UPDATE: {
            struct mxcfb_update_compat *update = argument;
            convert_and_submit(update->update_region.left, update->update_region.top,
                               update->update_region.width, update->update_region.height,
                               update->waveform_mode, update->update_mode);
            return 0;
        }
        default:
            // FBInk probes a number of device-specific controls. A no-op is
            // safer than leaking the memfd to the real kernel framebuffer API.
            return 0;
    }
}
