// An indirect branch whose target can't be found by analysis (computed from
// argc), but which has a BTI landing pad, as every indirect-branch target
// does in binaries built with branch protection (Ubuntu's). Run with two
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
	bti	j			// argc = 3 lands here
	mov	x0, #42
exit:
	mov	x8, #94			// exit_group
	svc	#0
