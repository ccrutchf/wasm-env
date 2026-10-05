// OpenCL's image builtins, for images kept in buffers (see opencl.rs,
// "Images"). An image is a buffer of its pixels, packed: row after row,
// slice (or array layer) after slice. Its header, 16 words, is in the
// argument block, after the image argument's buffer offset (H, below).
//
// Kernels' image and sampler handles become a pointer to the pixels and the
// sampler's bits (opencl.rs rewrites them), and their image builtins calls
// to these functions: __img_<builtin>_<image type>[_<variant>], the variant
// saying whether a sampler is given (s) or not (n) and the coordinates'
// type (i or f).

#pragma OPENCL EXTENSION cl_khr_fp16 : enable

typedef global uint *img;

// The header (written by the OpenCL driver, packages/opencl/icd.c), and a
// word of an image's: the translator's (gpu/src/wgsl.rs), which reads it
// from the argument block for the image argument the pointer came from.
enum {
	H_WIDTH, H_HEIGHT, H_DEPTH, H_ARRAY_SIZE, H_ROW_PITCH, H_SLICE_PITCH,
	H_ORDER, H_TYPE, H_ELEMENT_SIZE, H_CHANNELS,
};
uint __attribute__((overloadable, const)) __wenv_image_header(img i, uint field);
#define H(i, field) __wenv_image_header(i, field)

// Sampler bits, as __translate_sampler_initializer takes them.
#define S_NORMALIZED 1u
#define S_ADDRESS 0xEu
#define S_NONE 0u
#define S_CLAMP_TO_EDGE 2u
#define S_CLAMP 4u
#define S_REPEAT 6u
#define S_MIRRORED_REPEAT 8u
#define S_LINEAR 0x20u

uint __translate_sampler_initializer(uint s)
{
	return s;
}

// The extent in each dimension: width, height, and depth or layers.
static int4 extent(img i)
{
	return (int4)((int)H(i, H_WIDTH), (int)H(i, H_HEIGHT), (int)max(H(i, H_DEPTH), H(i, H_ARRAY_SIZE)), 1);
}

// A texel (x, y, and z or layer), in range.
static global uchar *texel(img i, int4 c)
{
	return (global uchar *)i + (uint)c.z * H(i, H_SLICE_PITCH) + (uint)c.y * H(i, H_ROW_PITCH) + (uint)c.x * H(i, H_ELEMENT_SIZE);
}

static int in_range(img i, int4 c)
{
	int4 e = extent(i);
	return c.x >= 0 && c.y >= 0 && c.z >= 0 && c.x < e.x && c.y < e.y && c.z < e.z;
}

// Stored channels to (r, g, b, a), and back, by the channel order.
#define TO_RGBA(T, one)                                                                                                \
	static T##4 to_rgba_##T(uint order, T##4 c)                                                                    \
	{                                                                                                              \
		switch (order) {                                                                                       \
		case CLK_R:                                                                                            \
		case CLK_Rx:                                                                                           \
			return (T##4)(c.x, 0, 0, one);                                                                 \
		case CLK_A:                                                                                            \
			return (T##4)(0, 0, 0, c.x);                                                                   \
		case CLK_RG:                                                                                           \
		case CLK_RGx:                                                                                          \
			return (T##4)(c.x, c.y, 0, one);                                                               \
		case CLK_RA:                                                                                           \
			return (T##4)(c.x, 0, 0, c.y);                                                                 \
		case CLK_RGB:                                                                                          \
		case CLK_RGBx:                                                                                         \
			return (T##4)(c.x, c.y, c.z, one);                                                             \
		case CLK_BGRA:                                                                                         \
			return c.zyxw;                                                                                 \
		case CLK_ARGB:                                                                                         \
			return c.yzwx;                                                                                 \
		case CLK_INTENSITY:                                                                                    \
			return c.xxxx;                                                                                 \
		case CLK_LUMINANCE:                                                                                    \
			return (T##4)(c.x, c.x, c.x, one);                                                             \
		default:                                                                                               \
			return c;                                                                                      \
		}                                                                                                      \
	}                                                                                                              \
	static T##4 from_rgba_##T(uint order, T##4 v)                                                                  \
	{                                                                                                              \
		switch (order) {                                                                                       \
		case CLK_A:                                                                                            \
			return v.wwww;                                                                                 \
		case CLK_RA:                                                                                           \
			return v.xwww;                                                                                 \
		case CLK_BGRA:                                                                                         \
			return v.zyxw;                                                                                 \
		case CLK_ARGB:                                                                                         \
			return v.wxyz;                                                                                 \
		default:                                                                                               \
			return v;                                                                                      \
		}                                                                                                      \
	}
TO_RGBA(float, 1.0f)
TO_RGBA(uint, 1u)

// Whether the order has alpha: CLAMP's border color is then all zeros, else
// opaque black.
static int has_alpha(uint order)
{
	return order == CLK_A || order == CLK_RA || order == CLK_RGBA || order == CLK_BGRA || order == CLK_ARGB ||
	       order == CLK_INTENSITY;
}

// A texel's channels, of a normalized, half or float format.
static float4 load_f(img i, global uchar *p)
{
	uint n = H(i, H_CHANNELS);
	float c[4] = {0.0f, 0.0f, 0.0f, 0.0f};
	switch (H(i, H_TYPE)) {
	case CLK_UNORM_INT8:
		for (uint k = 0; k < n; k++)
			c[k] = p[k] / 255.0f;
		break;
	case CLK_UNORM_INT16:
		for (uint k = 0; k < n; k++)
			c[k] = ((global ushort *)p)[k] / 65535.0f;
		break;
	case CLK_SNORM_INT8:
		for (uint k = 0; k < n; k++)
			c[k] = max(((global char *)p)[k] / 127.0f, -1.0f);
		break;
	case CLK_SNORM_INT16:
		for (uint k = 0; k < n; k++)
			c[k] = max(((global short *)p)[k] / 32767.0f, -1.0f);
		break;
	case CLK_UNORM_SHORT_565: {
		uint v = *(global ushort *)p;
		c[0] = (v >> 11) / 31.0f;
		c[1] = ((v >> 5) & 63) / 63.0f;
		c[2] = (v & 31) / 31.0f;
		break;
	}
	case CLK_UNORM_SHORT_555: {
		uint v = *(global ushort *)p;
		c[0] = ((v >> 10) & 31) / 31.0f;
		c[1] = ((v >> 5) & 31) / 31.0f;
		c[2] = (v & 31) / 31.0f;
		break;
	}
	case CLK_UNORM_INT_101010: {
		uint v = *(global uint *)p;
		c[0] = ((v >> 20) & 1023) / 1023.0f;
		c[1] = ((v >> 10) & 1023) / 1023.0f;
		c[2] = (v & 1023) / 1023.0f;
		break;
	}
	case CLK_HALF_FLOAT:
		for (uint k = 0; k < n; k++)
			c[k] = vload_half(k, (global half *)p);
		break;
	default: // CLK_FLOAT
		for (uint k = 0; k < n; k++)
			c[k] = ((global float *)p)[k];
	}
	return to_rgba_float(H(i, H_ORDER), (float4)(c[0], c[1], c[2], c[3]));
}

// A texel's channels, of an integer format (signed ones' bits as uint).
static uint4 load_u(img i, global uchar *p)
{
	uint n = H(i, H_CHANNELS);
	uint c[4] = {0, 0, 0, 0};
	for (uint k = 0; k < n; k++) {
		switch (H(i, H_TYPE)) {
		case CLK_SIGNED_INT8:
			c[k] = (uint)(int)((global char *)p)[k];
			break;
		case CLK_UNSIGNED_INT8:
			c[k] = p[k];
			break;
		case CLK_SIGNED_INT16:
			c[k] = (uint)(int)((global short *)p)[k];
			break;
		case CLK_UNSIGNED_INT16:
			c[k] = ((global ushort *)p)[k];
			break;
		default:
			c[k] = ((global uint *)p)[k];
		}
	}
	return to_rgba_uint(H(i, H_ORDER), (uint4)(c[0], c[1], c[2], c[3]));
}

static void store_f(img i, global uchar *p, float4 v)
{
	uint n = H(i, H_CHANNELS);
	float4 s = from_rgba_float(H(i, H_ORDER), v);
	float c[4] = {s.x, s.y, s.z, s.w};
	switch (H(i, H_TYPE)) {
	case CLK_UNORM_INT8:
		for (uint k = 0; k < n; k++)
			p[k] = convert_uchar_sat_rte(c[k] * 255.0f);
		break;
	case CLK_UNORM_INT16:
		for (uint k = 0; k < n; k++)
			((global ushort *)p)[k] = convert_ushort_sat_rte(c[k] * 65535.0f);
		break;
	case CLK_SNORM_INT8:
		for (uint k = 0; k < n; k++)
			((global char *)p)[k] = convert_char_sat_rte(c[k] * 127.0f);
		break;
	case CLK_SNORM_INT16:
		for (uint k = 0; k < n; k++)
			((global short *)p)[k] = convert_short_sat_rte(c[k] * 32767.0f);
		break;
	case CLK_UNORM_SHORT_565:
		*(global ushort *)p = (ushort)(min(convert_uint_sat_rte(c[0] * 31.0f), 31u) << 11 |
		                               min(convert_uint_sat_rte(c[1] * 63.0f), 63u) << 5 |
		                               min(convert_uint_sat_rte(c[2] * 31.0f), 31u));
		break;
	case CLK_UNORM_SHORT_555:
		*(global ushort *)p = (ushort)(min(convert_uint_sat_rte(c[0] * 31.0f), 31u) << 10 |
		                               min(convert_uint_sat_rte(c[1] * 31.0f), 31u) << 5 |
		                               min(convert_uint_sat_rte(c[2] * 31.0f), 31u));
		break;
	case CLK_UNORM_INT_101010:
		*(global uint *)p = min(convert_uint_sat_rte(c[0] * 1023.0f), 1023u) << 20 |
		                    min(convert_uint_sat_rte(c[1] * 1023.0f), 1023u) << 10 |
		                    min(convert_uint_sat_rte(c[2] * 1023.0f), 1023u);
		break;
	case CLK_HALF_FLOAT:
		for (uint k = 0; k < n; k++)
			vstore_half_rte(c[k], k, (global half *)p);
		break;
	default:
		for (uint k = 0; k < n; k++)
			((global float *)p)[k] = c[k];
	}
}

// Integers: `sign`, written by write_imagei (saturated to the channel's
// range by its signedness).
static void store_u(img i, global uchar *p, uint4 v, int sign)
{
	uint n = H(i, H_CHANNELS);
	uint4 s = from_rgba_uint(H(i, H_ORDER), v);
	uint c[4] = {s.x, s.y, s.z, s.w};
	for (uint k = 0; k < n; k++) {
		int si = (int)c[k];
		switch (H(i, H_TYPE)) {
		case CLK_SIGNED_INT8:
			((global char *)p)[k] = sign ? convert_char_sat(si) : convert_char_sat(c[k]);
			break;
		case CLK_UNSIGNED_INT8:
			p[k] = sign ? convert_uchar_sat(si) : convert_uchar_sat(c[k]);
			break;
		case CLK_SIGNED_INT16:
			((global short *)p)[k] = sign ? convert_short_sat(si) : convert_short_sat(c[k]);
			break;
		case CLK_UNSIGNED_INT16:
			((global ushort *)p)[k] = sign ? convert_ushort_sat(si) : convert_ushort_sat(c[k]);
			break;
		case CLK_SIGNED_INT32:
			((global int *)p)[k] = sign ? si : (int)min(c[k], 0x7FFFFFFFu);
			break;
		default:
			((global uint *)p)[k] = sign ? (uint)max(si, 0) : c[k];
		}
	}
}

// --- Addressing and filtering ---------------------------------------------

static int to_int(float u)
{
	return convert_int_sat_rtn(u);
}

// The texel index for coordinate s along an axis of `size` texels, nearest
// filtering (OpenCL 1.2, 8.2); out of range for CLAMP's border.
static int nearest(float s, int size, uint smp)
{
	uint mode = smp & S_ADDRESS;
	if (mode == S_REPEAT) {
		int k = to_int(floor((s - floor(s)) * size));
		return k > size - 1 ? k - size : k;
	}
	if (mode == S_MIRRORED_REPEAT) {
		float t = fabs(s - 2.0f * rint(0.5f * s));
		return min(to_int(floor(t * size)), size - 1);
	}
	float u = smp & S_NORMALIZED ? s * size : s;
	int k = to_int(floor(u));
	return mode == S_CLAMP ? k : clamp(k, 0, size - 1);
}

// The two texel indices and the weight of the second, linear filtering.
static void linear(float s, int size, uint smp, int *i0, int *i1, float *a)
{
	uint mode = smp & S_ADDRESS;
	float u;
	if (mode == S_REPEAT)
		u = (s - floor(s)) * size;
	else if (mode == S_MIRRORED_REPEAT)
		u = fabs(s - 2.0f * rint(0.5f * s)) * size;
	else
		u = smp & S_NORMALIZED ? s * size : s;
	float f = floor(u - 0.5f);
	*a = (u - 0.5f) - f;
	int k0 = to_int(f), k1 = k0 == 0x7FFFFFFF ? k0 : k0 + 1;
	if (mode == S_REPEAT) {
		if (k0 < 0)
			k0 += size;
		if (k1 > size - 1)
			k1 -= size;
	} else if (mode == S_MIRRORED_REPEAT) {
		k0 = max(k0, 0);
		k1 = min(k1, size - 1);
	} else if (mode != S_CLAMP) {
		k0 = clamp(k0, 0, size - 1);
		k1 = clamp(k1, 0, size - 1);
	}
	*i0 = k0;
	*i1 = k1;
}

// A texel's value for reads, or CLAMP's border color out of range.
static float4 fetch_f(img i, int4 c)
{
	if (!in_range(i, c))
		return has_alpha(H(i, H_ORDER)) ? (float4)(0.0f) : (float4)(0.0f, 0.0f, 0.0f, 1.0f);
	return load_f(i, texel(i, c));
}

static uint4 fetch_u(img i, int4 c)
{
	if (!in_range(i, c))
		return has_alpha(H(i, H_ORDER)) ? (uint4)(0) : (uint4)(0, 0, 0, 1);
	return load_u(i, texel(i, c));
}

// An array's layer from its coordinate.
static int layer(img i, float v)
{
	return clamp(to_int(rint(v)), 0, (int)H(i, H_ARRAY_SIZE) - 1);
}

// Integer coordinates with a sampler: unnormalized, nearest, addressed.
static int4 address_int(img i, uint smp, int4 c, int dims, int arrayed)
{
	int4 e = extent(i);
	if ((smp & S_ADDRESS) != S_CLAMP) {
		c.x = clamp(c.x, 0, e.x - 1);
		if (dims > 1)
			c.y = clamp(c.y, 0, e.y - 1);
		if (dims > 2 || arrayed)
			c.z = clamp(c.z, 0, e.z - 1);
	}
	if (arrayed)
		c.z = clamp(c.z, 0, e.z - 1);
	return c;
}

// Float coordinates (x, y, z; an array's layer in z): `dims` filtered.
static float4 sample_f(img i, uint smp, float4 c, int dims, int arrayed)
{
	int4 e = extent(i);
	int z_layer = arrayed ? layer(i, c.z) : 0;
	if (!(smp & S_LINEAR)) {
		int4 k = (int4)(nearest(c.x, e.x, smp), 0, z_layer, 0);
		if (dims > 1)
			k.y = nearest(c.y, e.y, smp);
		if (dims > 2)
			k.z = nearest(c.z, e.z, smp);
		return fetch_f(i, k);
	}
	int x0, x1, y0 = 0, y1 = 0, z0 = z_layer, z1 = z_layer;
	float a, b = 0.0f, g = 0.0f;
	linear(c.x, e.x, smp, &x0, &x1, &a);
	if (dims > 1)
		linear(c.y, e.y, smp, &y0, &y1, &b);
	if (dims > 2)
		linear(c.z, e.z, smp, &z0, &z1, &g);
	float4 r = (1 - a) * (1 - b) * (1 - g) * fetch_f(i, (int4)(x0, y0, z0, 0)) + a * (1 - b) * (1 - g) * fetch_f(i, (int4)(x1, y0, z0, 0));
	if (dims > 1)
		r += (1 - a) * b * (1 - g) * fetch_f(i, (int4)(x0, y1, z0, 0)) + a * b * (1 - g) * fetch_f(i, (int4)(x1, y1, z0, 0));
	if (dims > 2)
		r += (1 - a) * (1 - b) * g * fetch_f(i, (int4)(x0, y0, z1, 0)) + a * (1 - b) * g * fetch_f(i, (int4)(x1, y0, z1, 0)) +
		     (1 - a) * b * g * fetch_f(i, (int4)(x0, y1, z1, 0)) + a * b * g * fetch_f(i, (int4)(x1, y1, z1, 0));
	return r;
}

static uint4 sample_u(img i, uint smp, float4 c, int dims, int arrayed)
{
	int4 e = extent(i);
	int4 k = (int4)(nearest(c.x, e.x, smp), 0, arrayed ? layer(i, c.z) : 0, 0);
	if (dims > 1)
		k.y = nearest(c.y, e.y, smp);
	if (dims > 2)
		k.z = nearest(c.z, e.z, smp);
	return fetch_u(i, k);
}

// Samplerless reads and writes: coordinates in range (else the read gives
// the border color and the write is dropped, where OpenCL leaves both
// undefined).
static void write_f(img i, int4 c, float4 v)
{
	if (in_range(i, c))
		store_f(i, texel(i, c), v);
}

static void write_u(img i, int4 c, uint4 v, int sign)
{
	if (in_range(i, c))
		store_u(i, texel(i, c), v, sign);
}

// --- The builtins, per image type -------------------------------------------

// Coordinates as (x, y, z or layer).
#define I1(c) ((int4)((c), 0, 0, 0))
#define F1(c) ((float4)((c), 0.0f, 0.0f, 0.0f))
#define I1A(c) ((int4)((c).x, 0, (c).y, 0))
#define F1A(c) ((float4)((c).x, 0.0f, (c).y, 0.0f))
#define I2(c) ((int4)((c).x, (c).y, 0, 0))
#define F2(c) ((float4)((c).x, (c).y, 0.0f, 0.0f))
#define I3(c) ((int4)((c).x, (c).y, (c).z, 0))
#define F3(c) ((float4)((c).x, (c).y, (c).z, 0.0f))

#define GETS(kind)                                                                                                     \
	int __img_get_image_width_##kind(img i) { return (int)H(i, H_WIDTH); }                                         \
	int __img_get_image_height_##kind(img i) { return (int)H(i, H_HEIGHT); }                                       \
	int __img_get_image_depth_##kind(img i) { return (int)H(i, H_DEPTH); }                                         \
	uint __img_get_image_array_size_##kind(img i) { return H(i, H_ARRAY_SIZE); }                                   \
	int __img_get_image_channel_data_type_##kind(img i) { return (int)H(i, H_TYPE); }                              \
	int __img_get_image_channel_order_##kind(img i) { return (int)H(i, H_ORDER); }                                 \
	int2 __img_get_image_dim2_##kind(img i) { return (int2)((int)H(i, H_WIDTH), (int)H(i, H_HEIGHT)); }               \
	int4 __img_get_image_dim4_##kind(img i) { return (int4)((int)H(i, H_WIDTH), (int)H(i, H_HEIGHT), (int)H(i, H_DEPTH), 0); }

#define READS(kind, CI, CF, TI, TF, dims, arrayed)                                                                     \
	float4 __img_read_imagef_##kind##_s_i(img i, uint s, CI c) { return fetch_f(i, address_int(i, s, TI(c), dims, arrayed)); } \
	float4 __img_read_imagef_##kind##_s_f(img i, uint s, CF c) { return sample_f(i, s, TF(c), dims, arrayed); }  \
	float4 __img_read_imagef_##kind##_n_i(img i, CI c) { return fetch_f(i, TI(c)); }                           \
	int4 __img_read_imagei_##kind##_s_i(img i, uint s, CI c) { return as_int4(fetch_u(i, address_int(i, s, TI(c), dims, arrayed))); } \
	int4 __img_read_imagei_##kind##_s_f(img i, uint s, CF c) { return as_int4(sample_u(i, s, TF(c), dims, arrayed)); } \
	int4 __img_read_imagei_##kind##_n_i(img i, CI c) { return as_int4(fetch_u(i, TI(c))); }                     \
	uint4 __img_read_imageui_##kind##_s_i(img i, uint s, CI c) { return fetch_u(i, address_int(i, s, TI(c), dims, arrayed)); } \
	uint4 __img_read_imageui_##kind##_s_f(img i, uint s, CF c) { return sample_u(i, s, TF(c), dims, arrayed); }  \
	uint4 __img_read_imageui_##kind##_n_i(img i, CI c) { return fetch_u(i, TI(c)); }                            \
	half4 __img_read_imageh_##kind##_s_i(img i, uint s, CI c) { return convert_half4(fetch_f(i, address_int(i, s, TI(c), dims, arrayed))); } \
	half4 __img_read_imageh_##kind##_s_f(img i, uint s, CF c) { return convert_half4(sample_f(i, s, TF(c), dims, arrayed)); } \
	half4 __img_read_imageh_##kind##_n_i(img i, CI c) { return convert_half4(fetch_f(i, TI(c))); }

#define WRITES(kind, CI, TI)                                                                                           \
	void __img_write_imagef_##kind##_i(img i, CI c, float4 v) { write_f(i, TI(c), v); }                          \
	void __img_write_imagei_##kind##_i(img i, CI c, int4 v) { write_u(i, TI(c), as_uint4(v), 1); }               \
	void __img_write_imageui_##kind##_i(img i, CI c, uint4 v) { write_u(i, TI(c), v, 0); }                       \
	void __img_write_imageh_##kind##_i(img i, CI c, half4 v) { write_f(i, TI(c), convert_float4(v)); }

#define KIND(name, CI, CF, TI, TF, dims, arrayed)                                                                      \
	GETS(name##_ro)                                                                                                \
	GETS(name##_wo)                                                                                                \
	READS(name##_ro, CI, CF, TI, TF, dims, arrayed)                                                                \
	WRITES(name##_wo, CI, TI)

KIND(image1d, int, float, I1, F1, 1, 0)
KIND(image1d_buffer, int, float, I1, F1, 1, 0)
KIND(image1d_array, int2, float2, I1A, F1A, 1, 1)
KIND(image2d, int2, float2, I2, F2, 2, 0)
KIND(image2d_array, int4, float4, I3, F3, 2, 1)
KIND(image3d, int4, float4, I3, F3, 3, 0)
