// A plugin that greet.c loads with dlopen, and which calls back into
// libgreet.so, which it links against.
const char *greet(const char *);
static int initialized;
__attribute__((constructor)) static void init(void) { initialized = 7; }
int plugin_value(int x) { return x * 3 + initialized; }
const char *plugin_greet(void) { return greet("plugin"); }
