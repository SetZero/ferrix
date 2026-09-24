/*
 * The virtio-gpu numbers and layouts `libs/linux-abi/src/virtgpu.rs` writes
 * down, printed from the UAPI headers themselves.
 *
 * Run by `libs/linux-abi/probe/virtgpu.sh` on a Linux host with the kernel's
 * UAPI headers (`linux-libc-dev`) once natively for 64-bit and once for
 * ARMv7-A under qemu-arm, into `virtgpu-64.txt` and `virtgpu-32.txt` beside
 * it. The crate's tests read both files and require every line to match what
 * the crate says, so a number changed on either side fails `cargo test`.
 *
 * Each line is `name value`, in decimal. Layouts are `sizeof.<struct>` and
 * `offsetof.<struct>.<field>`.
 *
 * These are the render node's ioctls, which `/dev/dri/renderD<N>` answers and
 * `/dev/dri/card0` does not, so they are probed apart from `drm.c`: they come
 * from one driver's header rather than from DRM's own, and every one of them
 * is the same at both widths, because the structures carry their user
 * pointers as `__u64`.
 */

#include <stddef.h>
#include <stdio.h>
#include <drm/drm.h>
#include <drm/virtgpu_drm.h>

#define VALUE(name) printf("%s %llu\n", #name, (unsigned long long)(name))
#define SIZE(type) printf("sizeof.%s %zu\n", #type, sizeof(struct type))
#define FIELD(type, field) \
	printf("offsetof.%s.%s %zu\n", #type, #field, offsetof(struct type, field))

int main(void)
{
	/*
	 * The ioctls. Driver-private, so each number is `DRM_COMMAND_BASE`
	 * plus the driver's own index, and encodes its argument's size.
	 */
	VALUE(DRM_IOCTL_VIRTGPU_MAP);
	VALUE(DRM_IOCTL_VIRTGPU_EXECBUFFER);
	VALUE(DRM_IOCTL_VIRTGPU_GETPARAM);
	VALUE(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE);
	VALUE(DRM_IOCTL_VIRTGPU_RESOURCE_INFO);
	VALUE(DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST);
	VALUE(DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST);
	VALUE(DRM_IOCTL_VIRTGPU_WAIT);
	VALUE(DRM_IOCTL_VIRTGPU_GET_CAPS);
	VALUE(DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB);
	VALUE(DRM_IOCTL_VIRTGPU_CONTEXT_INIT);

	/* What GETPARAM answers about. */
	VALUE(VIRTGPU_PARAM_3D_FEATURES);
	VALUE(VIRTGPU_PARAM_CAPSET_QUERY_FIX);
	VALUE(VIRTGPU_PARAM_RESOURCE_BLOB);
	VALUE(VIRTGPU_PARAM_HOST_VISIBLE);
	VALUE(VIRTGPU_PARAM_CROSS_DEVICE);
	VALUE(VIRTGPU_PARAM_CONTEXT_INIT);
	VALUE(VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs);
	VALUE(VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME);

	/* The capability sets GET_CAPS may be asked for. */
	VALUE(VIRTGPU_DRM_CAPSET_VIRGL);
	VALUE(VIRTGPU_DRM_CAPSET_VIRGL2);
	VALUE(VIRTGPU_DRM_CAPSET_VENUS);

	SIZE(drm_virtgpu_getparam);
	FIELD(drm_virtgpu_getparam, param);
	FIELD(drm_virtgpu_getparam, value);

	SIZE(drm_virtgpu_context_init);
	FIELD(drm_virtgpu_context_init, num_params);
	FIELD(drm_virtgpu_context_init, pad);
	FIELD(drm_virtgpu_context_init, ctx_set_params);

	SIZE(drm_virtgpu_context_set_param);
	FIELD(drm_virtgpu_context_set_param, param);
	FIELD(drm_virtgpu_context_set_param, value);

	/* What CONTEXT_INIT's parameters may set. */
	VALUE(VIRTGPU_CONTEXT_PARAM_CAPSET_ID);
	VALUE(VIRTGPU_CONTEXT_PARAM_NUM_RINGS);
	VALUE(VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK);
	VALUE(VIRTGPU_CONTEXT_PARAM_DEBUG_NAME);

	SIZE(drm_virtgpu_resource_create);
	FIELD(drm_virtgpu_resource_create, target);
	FIELD(drm_virtgpu_resource_create, format);
	FIELD(drm_virtgpu_resource_create, bind);
	FIELD(drm_virtgpu_resource_create, width);
	FIELD(drm_virtgpu_resource_create, height);
	FIELD(drm_virtgpu_resource_create, depth);
	FIELD(drm_virtgpu_resource_create, array_size);
	FIELD(drm_virtgpu_resource_create, last_level);
	FIELD(drm_virtgpu_resource_create, nr_samples);
	FIELD(drm_virtgpu_resource_create, flags);
	FIELD(drm_virtgpu_resource_create, bo_handle);
	FIELD(drm_virtgpu_resource_create, res_handle);
	FIELD(drm_virtgpu_resource_create, size);
	FIELD(drm_virtgpu_resource_create, stride);

	SIZE(drm_virtgpu_resource_create_blob);
	FIELD(drm_virtgpu_resource_create_blob, blob_mem);
	FIELD(drm_virtgpu_resource_create_blob, blob_flags);
	FIELD(drm_virtgpu_resource_create_blob, bo_handle);
	FIELD(drm_virtgpu_resource_create_blob, res_handle);
	FIELD(drm_virtgpu_resource_create_blob, size);
	FIELD(drm_virtgpu_resource_create_blob, pad);
	FIELD(drm_virtgpu_resource_create_blob, cmd_size);
	FIELD(drm_virtgpu_resource_create_blob, cmd);
	FIELD(drm_virtgpu_resource_create_blob, blob_id);

	/* Where a blob's memory is, and what it may be used for. */
	VALUE(VIRTGPU_BLOB_MEM_GUEST);
	VALUE(VIRTGPU_BLOB_MEM_HOST3D);
	VALUE(VIRTGPU_BLOB_MEM_HOST3D_GUEST);
	VALUE(VIRTGPU_BLOB_FLAG_USE_MAPPABLE);
	VALUE(VIRTGPU_BLOB_FLAG_USE_SHAREABLE);
	VALUE(VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE);

	SIZE(drm_virtgpu_resource_info);
	FIELD(drm_virtgpu_resource_info, bo_handle);
	FIELD(drm_virtgpu_resource_info, res_handle);
	FIELD(drm_virtgpu_resource_info, size);
	FIELD(drm_virtgpu_resource_info, blob_mem);

	SIZE(drm_virtgpu_map);
	FIELD(drm_virtgpu_map, offset);
	FIELD(drm_virtgpu_map, handle);
	FIELD(drm_virtgpu_map, pad);

	SIZE(drm_virtgpu_get_caps);
	FIELD(drm_virtgpu_get_caps, cap_set_id);
	FIELD(drm_virtgpu_get_caps, cap_set_ver);
	FIELD(drm_virtgpu_get_caps, addr);
	FIELD(drm_virtgpu_get_caps, size);
	FIELD(drm_virtgpu_get_caps, pad);

	SIZE(drm_virtgpu_execbuffer);
	FIELD(drm_virtgpu_execbuffer, flags);
	FIELD(drm_virtgpu_execbuffer, size);
	FIELD(drm_virtgpu_execbuffer, command);
	FIELD(drm_virtgpu_execbuffer, bo_handles);
	FIELD(drm_virtgpu_execbuffer, num_bo_handles);
	FIELD(drm_virtgpu_execbuffer, fence_fd);
	FIELD(drm_virtgpu_execbuffer, ring_idx);
	FIELD(drm_virtgpu_execbuffer, syncobj_stride);
	FIELD(drm_virtgpu_execbuffer, num_in_syncobjs);
	FIELD(drm_virtgpu_execbuffer, num_out_syncobjs);
	FIELD(drm_virtgpu_execbuffer, in_syncobjs);
	FIELD(drm_virtgpu_execbuffer, out_syncobjs);

	/* What a submission may ask for, and which ring it runs on. */
	VALUE(VIRTGPU_EXECBUF_FENCE_FD_IN);
	VALUE(VIRTGPU_EXECBUF_FENCE_FD_OUT);
	VALUE(VIRTGPU_EXECBUF_RING_IDX);
	/* What a wait may ask for. */
	VALUE(VIRTGPU_WAIT_NOWAIT);

	SIZE(drm_virtgpu_3d_box);
	FIELD(drm_virtgpu_3d_box, x);
	FIELD(drm_virtgpu_3d_box, y);
	FIELD(drm_virtgpu_3d_box, z);
	FIELD(drm_virtgpu_3d_box, w);
	FIELD(drm_virtgpu_3d_box, h);
	FIELD(drm_virtgpu_3d_box, d);

	SIZE(drm_virtgpu_3d_transfer_to_host);
	FIELD(drm_virtgpu_3d_transfer_to_host, bo_handle);
	FIELD(drm_virtgpu_3d_transfer_to_host, box);
	FIELD(drm_virtgpu_3d_transfer_to_host, level);
	FIELD(drm_virtgpu_3d_transfer_to_host, offset);
	FIELD(drm_virtgpu_3d_transfer_to_host, stride);
	FIELD(drm_virtgpu_3d_transfer_to_host, layer_stride);

	SIZE(drm_virtgpu_3d_transfer_from_host);
	FIELD(drm_virtgpu_3d_transfer_from_host, bo_handle);
	FIELD(drm_virtgpu_3d_transfer_from_host, box);
	FIELD(drm_virtgpu_3d_transfer_from_host, level);
	FIELD(drm_virtgpu_3d_transfer_from_host, offset);
	FIELD(drm_virtgpu_3d_transfer_from_host, stride);
	FIELD(drm_virtgpu_3d_transfer_from_host, layer_stride);

	SIZE(drm_virtgpu_3d_wait);
	FIELD(drm_virtgpu_3d_wait, handle);
	FIELD(drm_virtgpu_3d_wait, flags);
	return 0;
}
