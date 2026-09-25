// glibc 2.39's (Ubuntu 24.04) NEON strrchr, as machine code: a regression
// test for lifting it (a NUL-detection mask built with bic/bit/umaxp).
	.text
	.global glibc_strrchr
	.p2align 6
glibc_strrchr:
	.inst	0xd503201f	// nop 
	.inst	0x927cec02	// and x2, x0, #0xfffffffffffffff0
	.inst	0x4e010c20	// dup v0.16b, w1
	.inst	0x4f01e664	// movi v4.16b, #0x33
	.inst	0x4c407041	// ld1 {v1.16b}, [x2]
	.inst	0x4e209822	// cmeq v2.16b, v1.16b, #0
	.inst	0x6e208c23	// cmeq v3.16b, v1.16b, v0.16b
	.inst	0x6ea41c62	// bit v2.16b, v3.16b, v4.16b
	.inst	0x0f0c8445	// shrn v5.8b, v2.8h, #4
	.inst	0xd37ef404	// lsl x4, x0, #2
	.inst	0x9e6600a3	// fmov x3, d5
	.inst	0x9ac42463	// lsr x3, x3, x4
	.inst	0x9ac42063	// lsl x3, x3, x4
	.inst	0xf202e465	// ands x5, x3, #0xcccccccccccccccc
	.inst	0x540002c1	// b.ne 9e610 <strrchr@@GLIBC_2.17+0x90>  // b.any
	.inst	0xb50003e3	// cbnz x3, 9e638 <strrchr@@GLIBC_2.17+0xb8>
	.inst	0x3dc00441	// ldr q1, [x2, #16]
	.inst	0x6e208c23	// cmeq v3.16b, v1.16b, v0.16b
	.inst	0x6e213c62	// cmhs v2.16b, v3.16b, v1.16b
	.inst	0x6e22a445	// umaxp v5.16b, v2.16b, v2.16b
	.inst	0x9e6600a3	// fmov x3, d5
	.inst	0xb5000103	// cbnz x3, 9e5f4 <strrchr@@GLIBC_2.17+0x74>
	.inst	0x3cc20c41	// ldr q1, [x2, #32]!
	.inst	0x6e208c23	// cmeq v3.16b, v1.16b, v0.16b
	.inst	0x6e213c62	// cmhs v2.16b, v3.16b, v1.16b
	.inst	0x6e22a445	// umaxp v5.16b, v2.16b, v2.16b
	.inst	0x9e6600a3	// fmov x3, d5
	.inst	0xb4fffea3	// cbz x3, 9e5c0 <strrchr@@GLIBC_2.17+0x40>
	.inst	0xd1004042	// sub x2, x2, #0x10
	.inst	0x91004042	// add x2, x2, #0x10
	.inst	0x4e209822	// cmeq v2.16b, v1.16b, #0
	.inst	0x6ea41c62	// bit v2.16b, v3.16b, v4.16b
	.inst	0x0f0c8445	// shrn v5.8b, v2.8h, #4
	.inst	0x9e6600a3	// fmov x3, d5
	.inst	0xf202e465	// ands x5, x3, #0xcccccccccccccccc
	.inst	0x54000160	// b.eq 9e638 <strrchr@@GLIBC_2.17+0xb8>  // b.none
	.inst	0xd10004a5	// sub x5, x5, #0x1
	.inst	0x9200e466	// and x6, x3, #0x3333333333333333
	.inst	0xea0500c6	// ands x6, x6, x5
	.inst	0x91003c40	// add x0, x2, #0xf
	.inst	0xdac010c3	// clz x3, x6
	.inst	0xcb430800	// sub x0, x0, x3, lsr #2
	.inst	0x9a9f1000	// csel x0, x0, xzr, ne
	.inst	0xd65f03c0	// ret 
	.inst	0xd503201f	// nop 
	.inst	0xd503201f	// nop 
	.inst	0x91004042	// add x2, x2, #0x10
	.inst	0x6f079604	// bic v4.8h, #0xf0
	.inst	0xf100007f	// cmp x3, #0x0
	.inst	0x9a841044	// csel x4, x2, x4, ne
	.inst	0x9a861066	// csel x6, x3, x6, ne
	.inst	0x4cdf7041	// ld1 {v1.16b}, [x2], #16
	.inst	0x4e209822	// cmeq v2.16b, v1.16b, #0
	.inst	0x6e208c23	// cmeq v3.16b, v1.16b, v0.16b
	.inst	0x6ea41c62	// bit v2.16b, v3.16b, v4.16b
	.inst	0x6e22a445	// umaxp v5.16b, v2.16b, v2.16b
	.inst	0x9e6600a3	// fmov x3, d5
	.inst	0xf202e47f	// tst x3, #0xcccccccccccccccc
	.inst	0x54fffec0	// b.eq 9e640 <strrchr@@GLIBC_2.17+0xc0>  // b.none
	.inst	0x6f00b5e2	// bic v2.8h, #0xf, lsl #8
	.inst	0x4e22bc45	// addp v5.16b, v2.16b, v2.16b
	.inst	0x9e6600a3	// fmov x3, d5
	.inst	0x9202e465	// and x5, x3, #0xcccccccccccccccc
	.inst	0xd10004a5	// sub x5, x5, #0x1
	.inst	0x9200e463	// and x3, x3, #0x3333333333333333
	.inst	0xea050063	// ands x3, x3, x5
	.inst	0x9a861066	// csel x6, x3, x6, ne
	.inst	0x9a841044	// csel x4, x2, x4, ne
	.inst	0xd1000484	// sub x4, x4, #0x1
	.inst	0xdac010c3	// clz x3, x6
	.inst	0xcb430880	// sub x0, x4, x3, lsr #2
	.inst	0xd65f03c0	// ret 
