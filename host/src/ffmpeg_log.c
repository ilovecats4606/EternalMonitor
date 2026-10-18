#include <libavutil/log.h>

/* Keep va_list in C: its ABI differs between MSVC, Intel and Apple ARM. */
static void (*log_sink)(int, const char *);

static void forward_log(void *context, int level, const char *format, va_list args) {
    if (level > AV_LOG_WARNING || !log_sink) return;
    char line[4096];
    int prefix = 1;
    int length = av_log_format_line2(context, level, format, args, line, sizeof(line), &prefix);
    if (length >= 0) log_sink(level, line);
}

void eternal_install_ffmpeg_logger(void (*sink)(int, const char *)) {
    log_sink = sink;
    av_log_set_level(AV_LOG_WARNING);
    av_log_set_callback(forward_log);
}
