// A jump table whose address is set before a loop, where the code just
// before the dispatch sets the same register on a different path (as in
// OpenSSL's EVP_Q_digest): the table address must be found through the
// branch that enters the dispatch, not the code linearly above it.
// Exits with 1 + 20 + 100 + 20 = 141.
	.text
	.global _start
_start:
	adrp	x0, input
	add	x0, x0, :lo12:input
	mov	x19, #0
	adrp	x2, table		// the table, set up once
	add	x2, x2, :lo12:table
loop:
	ldr	w1, [x0]
	cbnz	w1, dispatch
	mov	x0, x19
	mov	x8, #94			// exit_group
	svc	#0
other:
	mov	x2, #0			// a different path, linearly adjacent
	b	loop
dispatch:
	sub	w1, w1, #10
	cmp	w1, #3
	b.hi	next
	ldrb	w1, [x2, w1, uxtw]
	adr	x3, base
	add	x1, x3, w1, sxtb #2
	br	x1
case_before:
	add	x19, x19, #20
	b	next
base:
	add	x19, x19, #1
	b	next
case2:
	add	x19, x19, #100
next:
	add	x0, x0, #4
	b	loop

	.section .rodata
input:
	.word	10, 11, 12, 13, 99, 0
table:
	.byte	(base - base) / 4, (case_before - base) / 4, (case2 - base) / 4, (case_before - base) / 4
