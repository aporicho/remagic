#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <linux/uinput.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

enum {
    SCREEN_WIDTH = 954,
    SCREEN_HEIGHT = 1696,
    DEFAULT_HOTPLUG_MS = 1500,
    DEFAULT_HOLD_MS = 120,
    DEFAULT_SETTLE_MS = 500,
};

static void sleep_ms(unsigned int milliseconds)
{
    struct timespec delay = {
        .tv_sec = (time_t)(milliseconds / 1000U),
        .tv_nsec = (long)(milliseconds % 1000U) * 1000000L,
    };

    while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {
    }
}

static int parse_number(const char *text, long minimum, long maximum, long *value)
{
    char *end = NULL;
    long parsed;

    errno = 0;
    parsed = strtol(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || parsed < minimum || parsed > maximum)
        return -1;
    *value = parsed;
    return 0;
}

static int enable_capability(int fd, unsigned long request, int value, const char *name)
{
    if (ioctl(fd, request, value) == 0)
        return 0;
    fprintf(stderr, "remagic-uinput-tap: cannot enable %s: %s\n", name, strerror(errno));
    return -1;
}

static int emit_event(int fd, uint16_t type, uint16_t code, int32_t value)
{
    struct input_event event;
    const unsigned char *cursor;
    size_t remaining;

    memset(&event, 0, sizeof(event));
    gettimeofday(&event.time, NULL);
    event.type = type;
    event.code = code;
    event.value = value;

    cursor = (const unsigned char *)&event;
    remaining = sizeof(event);
    while (remaining > 0) {
        ssize_t written = write(fd, cursor, remaining);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            fprintf(stderr, "remagic-uinput-tap: event write failed: %s\n", strerror(errno));
            return -1;
        }
        cursor += (size_t)written;
        remaining -= (size_t)written;
    }
    return 0;
}

static int emit_sync(int fd)
{
    return emit_event(fd, EV_SYN, SYN_REPORT, 0);
}

static int emit_press(int fd, int x, int y)
{
    return emit_event(fd, EV_ABS, ABS_MT_SLOT, 0) ||
           emit_event(fd, EV_ABS, ABS_MT_TRACKING_ID, 1) ||
           emit_event(fd, EV_ABS, ABS_MT_POSITION_X, x) ||
           emit_event(fd, EV_ABS, ABS_MT_POSITION_Y, y) ||
           emit_event(fd, EV_ABS, ABS_MT_TOUCH_MAJOR, 8) ||
           emit_event(fd, EV_ABS, ABS_MT_PRESSURE, 64) ||
           emit_event(fd, EV_ABS, ABS_X, x) ||
           emit_event(fd, EV_ABS, ABS_Y, y) ||
           emit_event(fd, EV_KEY, BTN_TOOL_FINGER, 1) ||
           emit_event(fd, EV_KEY, BTN_TOUCH, 1) ||
           emit_sync(fd);
}

static int emit_release(int fd)
{
    return emit_event(fd, EV_ABS, ABS_MT_SLOT, 0) ||
           emit_event(fd, EV_ABS, ABS_MT_PRESSURE, 0) ||
           emit_event(fd, EV_ABS, ABS_MT_TRACKING_ID, -1) ||
           emit_event(fd, EV_KEY, BTN_TOUCH, 0) ||
           emit_event(fd, EV_KEY, BTN_TOOL_FINGER, 0) ||
           emit_sync(fd);
}

static int emit_pen_proximity(int fd, int x, int y)
{
    return emit_event(fd, EV_ABS, ABS_X, x) ||
           emit_event(fd, EV_ABS, ABS_Y, y) ||
           emit_event(fd, EV_ABS, ABS_PRESSURE, 0) ||
           emit_event(fd, EV_KEY, BTN_TOOL_PEN, 1) ||
           emit_sync(fd);
}

static int emit_pen_point(int fd, int x, int y, int pressure, bool touching)
{
    return emit_event(fd, EV_ABS, ABS_X, x) ||
           emit_event(fd, EV_ABS, ABS_Y, y) ||
           emit_event(fd, EV_ABS, ABS_PRESSURE, pressure) ||
           emit_event(fd, EV_KEY, BTN_TOUCH, touching ? 1 : 0) ||
           emit_sync(fd);
}

static int emit_pen_release(int fd, int x, int y)
{
    return emit_pen_point(fd, x, y, 0, false) ||
           emit_event(fd, EV_KEY, BTN_TOOL_PEN, 0) ||
           emit_sync(fd);
}

static int draw_pen_segment(int fd, int x1, int y1, int x2, int y2,
                            unsigned int duration_ms)
{
    const int steps = 16;
    int step;

    for (step = 0; step <= steps; ++step) {
        const int x = x1 + ((x2 - x1) * step) / steps;
        const int y = y1 + ((y2 - y1) * step) / steps;
        if (emit_pen_point(fd, x, y, 2048, true) < 0)
            return -1;
        sleep_ms(duration_ms / (unsigned int)steps);
    }
    if (emit_pen_point(fd, x2, y2, 0, false) < 0)
        return -1;
    sleep_ms(45);
    return 0;
}

static int draw_test_equation(int fd, unsigned int stroke_ms)
{
    static const int strokes[][4] = {
        /* A large handwritten 1 + 1 =, kept in one uinput lifetime. */
        {210, 570, 250, 520},
        {250, 520, 250, 810},
        {350, 665, 510, 665},
        {430, 585, 430, 745},
        {600, 570, 640, 520},
        {640, 520, 640, 810},
        {730, 625, 875, 625},
        {730, 705, 875, 705},
    };
    size_t index;

    if (emit_pen_proximity(fd, strokes[0][0], strokes[0][1]) < 0)
        return -1;
    sleep_ms(50);
    for (index = 0; index < sizeof(strokes) / sizeof(strokes[0]); ++index) {
        if (draw_pen_segment(fd,
                             strokes[index][0], strokes[index][1],
                             strokes[index][2], strokes[index][3],
                             stroke_ms) < 0)
            return -1;
    }
    return emit_pen_release(fd, strokes[index - 1][2], strokes[index - 1][3]);
}

static int open_uinput(void)
{
    int fd = open("/dev/uinput", O_WRONLY | O_CLOEXEC);
    if (fd < 0 && errno == ENOENT)
        fd = open("/dev/input/uinput", O_WRONLY | O_CLOEXEC);
    return fd;
}

int main(int argc, char **argv)
{
    struct uinput_user_dev device;
    long x;
    long y;
    long x2 = 0;
    long y2 = 0;
    long hotplug_ms = DEFAULT_HOTPLUG_MS;
    long hold_ms = DEFAULT_HOLD_MS;
    long settle_ms = DEFAULT_SETTLE_MS;
    int fd;
    bool created = false;
    bool pen_line_mode = argc > 1 && strcmp(argv[1], "--pen-line") == 0;
    bool pen_equation_mode = argc > 1 && strcmp(argv[1], "--pen-equation") == 0;
    bool pen_mode = pen_line_mode || pen_equation_mode;
    int result = EXIT_FAILURE;

    if ((!pen_mode &&
         (argc < 3 || argc > 6 ||
          parse_number(argv[1], 0, SCREEN_WIDTH - 1, &x) < 0 ||
          parse_number(argv[2], 0, SCREEN_HEIGHT - 1, &y) < 0 ||
          (argc >= 4 && parse_number(argv[3], 0, 30000, &hotplug_ms) < 0) ||
          (argc >= 5 && parse_number(argv[4], 1, 30000, &hold_ms) < 0) ||
          (argc >= 6 && parse_number(argv[5], 0, 30000, &settle_ms) < 0))) ||
        (pen_line_mode &&
         (argc < 6 || argc > 9 ||
          parse_number(argv[2], 0, SCREEN_WIDTH - 1, &x) < 0 ||
          parse_number(argv[3], 0, SCREEN_HEIGHT - 1, &y) < 0 ||
          parse_number(argv[4], 0, SCREEN_WIDTH - 1, &x2) < 0 ||
          parse_number(argv[5], 0, SCREEN_HEIGHT - 1, &y2) < 0 ||
          (argc >= 7 && parse_number(argv[6], 0, 30000, &hotplug_ms) < 0) ||
          (argc >= 8 && parse_number(argv[7], 1, 30000, &hold_ms) < 0) ||
          (argc >= 9 && parse_number(argv[8], 0, 30000, &settle_ms) < 0))) ||
        (pen_equation_mode &&
         (argc > 5 ||
          (argc >= 3 && parse_number(argv[2], 0, 30000, &hotplug_ms) < 0) ||
          (argc >= 4 && parse_number(argv[3], 1, 30000, &hold_ms) < 0) ||
          (argc >= 5 && parse_number(argv[4], 0, 30000, &settle_ms) < 0)))) {
        fprintf(stderr,
                "usage: %s X Y [hotplug-ms [hold-ms [settle-ms]]]\n"
                "       %s --pen-line X1 Y1 X2 Y2 [hotplug-ms [duration-ms [settle-ms]]]\n"
                "       %s --pen-equation [hotplug-ms [stroke-ms [settle-ms]]]\n"
                "       coordinates: 0..%d by 0..%d\n",
                argv[0], argv[0], argv[0], SCREEN_WIDTH - 1, SCREEN_HEIGHT - 1);
        return EXIT_FAILURE;
    }

    if (pen_line_mode && argc < 8)
        hold_ms = 500;
    if (pen_equation_mode && argc < 4)
        hold_ms = 120;

    fd = open_uinput();
    if (fd < 0) {
        fprintf(stderr, "remagic-uinput-tap: cannot open uinput: %s\n", strerror(errno));
        return EXIT_FAILURE;
    }

    if (enable_capability(fd, UI_SET_EVBIT, EV_SYN, "EV_SYN") < 0 ||
        enable_capability(fd, UI_SET_EVBIT, EV_KEY, "EV_KEY") < 0 ||
        enable_capability(fd, UI_SET_KEYBIT, BTN_TOUCH, "BTN_TOUCH") < 0 ||
        enable_capability(fd, UI_SET_EVBIT, EV_ABS, "EV_ABS") < 0 ||
        enable_capability(fd, UI_SET_ABSBIT, ABS_X, "ABS_X") < 0 ||
        enable_capability(fd, UI_SET_ABSBIT, ABS_Y, "ABS_Y") < 0 ||
        enable_capability(fd, UI_SET_PROPBIT, INPUT_PROP_DIRECT, "INPUT_PROP_DIRECT") < 0)
        goto out;

    if (pen_mode) {
        if (enable_capability(fd, UI_SET_KEYBIT, BTN_TOOL_PEN, "BTN_TOOL_PEN") < 0 ||
            enable_capability(fd, UI_SET_ABSBIT, ABS_PRESSURE, "ABS_PRESSURE") < 0)
            goto out;
    } else if (enable_capability(fd, UI_SET_KEYBIT, BTN_TOOL_FINGER, "BTN_TOOL_FINGER") < 0 ||
               enable_capability(fd, UI_SET_ABSBIT, ABS_MT_SLOT, "ABS_MT_SLOT") < 0 ||
               enable_capability(fd, UI_SET_ABSBIT, ABS_MT_TRACKING_ID, "ABS_MT_TRACKING_ID") < 0 ||
               enable_capability(fd, UI_SET_ABSBIT, ABS_MT_POSITION_X, "ABS_MT_POSITION_X") < 0 ||
               enable_capability(fd, UI_SET_ABSBIT, ABS_MT_POSITION_Y, "ABS_MT_POSITION_Y") < 0 ||
               enable_capability(fd, UI_SET_ABSBIT, ABS_MT_TOUCH_MAJOR, "ABS_MT_TOUCH_MAJOR") < 0 ||
               enable_capability(fd, UI_SET_ABSBIT, ABS_MT_PRESSURE, "ABS_MT_PRESSURE") < 0) {
        goto out;
    }

    memset(&device, 0, sizeof(device));
    snprintf(device.name, sizeof(device.name), "%s",
             pen_mode ? "Remagic acceptance marker"
                      : "Remagic acceptance touchscreen");
    device.id.bustype = BUS_USB;
    device.id.vendor = 0x524d;
    device.id.product = 0x0001;
    device.id.version = 1;
    device.absmin[ABS_X] = 0;
    device.absmax[ABS_X] = SCREEN_WIDTH - 1;
    device.absmin[ABS_Y] = 0;
    device.absmax[ABS_Y] = SCREEN_HEIGHT - 1;
    if (pen_mode) {
        device.absmin[ABS_PRESSURE] = 0;
        device.absmax[ABS_PRESSURE] = 4095;
    } else {
        device.absmin[ABS_MT_SLOT] = 0;
        device.absmax[ABS_MT_SLOT] = 9;
        device.absmin[ABS_MT_TRACKING_ID] = 0;
        device.absmax[ABS_MT_TRACKING_ID] = 65535;
        device.absmin[ABS_MT_POSITION_X] = 0;
        device.absmax[ABS_MT_POSITION_X] = SCREEN_WIDTH - 1;
        device.absmin[ABS_MT_POSITION_Y] = 0;
        device.absmax[ABS_MT_POSITION_Y] = SCREEN_HEIGHT - 1;
        device.absmin[ABS_MT_TOUCH_MAJOR] = 0;
        device.absmax[ABS_MT_TOUCH_MAJOR] = 255;
        device.absmin[ABS_MT_PRESSURE] = 0;
        device.absmax[ABS_MT_PRESSURE] = 255;
    }

    if (write(fd, &device, sizeof(device)) != (ssize_t)sizeof(device)) {
        fprintf(stderr, "remagic-uinput-tap: device setup failed: %s\n", strerror(errno));
        goto out;
    }
    if (ioctl(fd, UI_DEV_CREATE) < 0) {
        fprintf(stderr, "remagic-uinput-tap: device creation failed: %s\n", strerror(errno));
        goto out;
    }
    created = true;

    /* Give Qt/libinput enough time to receive the udev hotplug notification. */
    sleep_ms((unsigned int)hotplug_ms);
    if (pen_line_mode) {
        if (emit_pen_proximity(fd, (int)x, (int)y) < 0)
            goto out;
        sleep_ms(50);
        if (draw_pen_segment(fd, (int)x, (int)y, (int)x2, (int)y2,
                             (unsigned int)hold_ms) < 0)
            goto out;
        if (emit_pen_release(fd, (int)x2, (int)y2) < 0)
            goto out;
    } else if (pen_equation_mode) {
        if (draw_test_equation(fd, (unsigned int)hold_ms) < 0)
            goto out;
    } else {
        if (emit_press(fd, (int)x, (int)y) < 0)
            goto out;
        sleep_ms((unsigned int)hold_ms);
        if (emit_release(fd) < 0)
            goto out;
    }
    /* Keep the input node alive until Qt has consumed the release frame. */
    sleep_ms((unsigned int)settle_ms);
    result = EXIT_SUCCESS;

out:
    if (created && ioctl(fd, UI_DEV_DESTROY) < 0) {
        fprintf(stderr, "remagic-uinput-tap: device destruction failed: %s\n", strerror(errno));
        result = EXIT_FAILURE;
    }
    close(fd);
    return result;
}
