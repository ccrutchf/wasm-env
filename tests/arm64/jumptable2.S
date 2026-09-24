// A jump table whose address is set up before a loop, outside the
// straight-line code leading to the dispatch (seen in OpenSSL): the loop
// head is reached by a branch, and an early return separates it from the
// setup. Byte entries, sign-extended. Exits with the sum of the cases
// taken: 1 + 20 + 100 + 20 = 141.
	.text
	.global _start
_start:
	adrp	x0, input
	add	x0, x0, :lo12:input
	mov	x19, #0
	bl	walk
	mov	x0, x19
	mov	x8, #94
	svc	#0

walk:
	adrp	x2, table		// set up once, before the loop
	add	x2, x2, :lo12:table
loop:
	ldr	w1, [x0]
	cbnz	w1, 1f
	ret				// end of the list
1:	sub	w1, w1, #10
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
	.word	10, 11, 12, 13, 99, 0	// cases 0..3, then an out-of-range value
table:
	.byte	(base - base) / 4, (case_before - base) / 4, (case2 - base) / 4, (case_before - base) / 4
