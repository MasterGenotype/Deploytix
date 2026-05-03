#include <alsa/asoundlib.h>

static void noop_handler(
    const char *file, int line, const char *function,
    int err, const char *fmt, ...)
{
    (void)file; (void)line; (void)function; (void)err; (void)fmt;
}

void deploytix_suppress_alsa_errors(void)
{
    snd_lib_error_set_handler(noop_handler);
}
