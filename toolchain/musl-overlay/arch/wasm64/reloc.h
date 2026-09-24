// wasm-env: static linking only. dynlink.h needs these definitions to
// compile, but nothing here is used.
#define LDSO_ARCH "wasm64"

#define NO_LEGACY_INITFINI

#define TPOFF_K 0

#define REL_SYMBOLIC    1
#define REL_GOT         2
#define REL_PLT         3
#define REL_RELATIVE    4
#define REL_COPY        5
#define REL_DTPMOD      6
#define REL_DTPOFF      7
#define REL_TPOFF       8

#define CRTJMP(pc,sp) __builtin_trap()
