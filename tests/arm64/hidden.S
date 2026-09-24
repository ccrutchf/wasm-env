// A jump target no static analysis can find: the address is computed from
// argc at run time. The first run under wasm-env misses it and records it;
// the next translation includes it (tests/m2-exec.sh). Run with two
// arguments (argc = 3), it exits with 42.
	.text
	.global _start
_start:
	ldr	x1, [sp]		// argc
	lsl	x1, x1, #3
	adr	x0, anchor
	add	x0, x0, x1		// anchor + 8 * argc
	br	x0
anchor:
	mov	x0, #1
	b	exit
	mov	x0, #2
	b	exit
	mov	x0, #3
	b	exit
	mov	x0, #42			// argc = 3
	b	exit
exit:
	mov	x8, #94			// exit_group
	svc	#0
