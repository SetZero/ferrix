/*
 * The self-test build's stand-in for RM: every nvUvmInterface call that
 * UVM makes into the resource manager, answered as by an RM with no GPU.
 * Generated from nv_uvm_interface.h by the C0a session; in nvrm the real
 * calls go to kernel-open/nvidia/nv_uvm_interface.c and RM.
 *
 * A session can be made and UVM's callbacks registered, so UVM starts;
 * every other call says the operation is not supported, which UVM only
 * reaches through a GPU, and there is none.
 *
 * SPDX-License-Identifier: MIT
 */
#include "uvm_linux.h"
#include "nv_uvm_interface.h"

NV_STATUS nvUvmInterfaceRegisterGpu(const NvProcessorUuid *gpuUuid, UvmGpuPlatformInfo *gpuInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceUnregisterGpu(const NvProcessorUuid *gpuUuid)
{
}

NV_STATUS nvUvmInterfaceSessionCreate(uvmGpuSessionHandle *session,
                                      UvmPlatformInfo *platformInfo)
{
    *session = (uvmGpuSessionHandle)(unsigned long)1;
    memset(platformInfo, 0, sizeof(*platformInfo));
    return NV_OK;
}

NV_STATUS nvUvmInterfaceSessionDestroy(uvmGpuSessionHandle session)
{
    return NV_OK;
}

NV_STATUS nvUvmInterfaceDeviceCreate(uvmGpuSessionHandle session,
                                     const UvmGpuInfo *pGpuInfo,
                                     const NvProcessorUuid *gpuUuid,
                                     uvmGpuDeviceHandle *device,
                                     NvBool bCreateSmcPartition)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceDeviceDestroy(uvmGpuDeviceHandle device)
{
}

NV_STATUS nvUvmInterfaceAddressSpaceCreate(uvmGpuDeviceHandle device,
                                           unsigned long long vaBase,
                                           unsigned long long vaSize,
                                           NvBool enableAts,
                                           uvmGpuAddressSpaceHandle *vaSpace,
                                           UvmGpuAddressSpaceInfo *vaSpaceInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceDupAddressSpace(uvmGpuDeviceHandle device,
                                        NvHandle hUserClient,
                                        NvHandle hUserVASpace,
                                        uvmGpuAddressSpaceHandle *vaSpace,
                                        UvmGpuAddressSpaceInfo *vaSpaceInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceAddressSpaceDestroy(uvmGpuAddressSpaceHandle vaSpace)
{
}

NV_STATUS nvUvmInterfaceMemoryAllocFB(uvmGpuAddressSpaceHandle vaSpace,
                                      NvLength length,
                                      UvmGpuPointer * gpuPointer,
                                      UvmGpuAllocInfo * allocInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceMemoryAllocSys(uvmGpuAddressSpaceHandle vaSpace,
                                       NvLength length,
                                       UvmGpuPointer * gpuPointer,
                                       UvmGpuAllocInfo * allocInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceGetP2PCaps(uvmGpuDeviceHandle device1,
                                   uvmGpuDeviceHandle device2,
                                   UvmGpuP2PCapsParams * p2pCapsParams)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceGetPmaObject(uvmGpuDeviceHandle device,
                                     void **pPma,
                                     const UvmPmaStatistics **pPmaPubStats)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfacePmaRegisterEvictionCallbacks(void *pPma,
                                                     uvmPmaEvictPagesCallback evictPages,
                                                     uvmPmaEvictRangeCallback evictRange,
                                                     void *callbackData)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfacePmaUnregisterEvictionCallbacks(void *pPma)
{
}

NV_STATUS nvUvmInterfacePmaAllocPages(void *pPma,
                                      NvLength pageCount,
                                      NvU64 pageSize,
                                      UvmPmaAllocationOptions *pPmaAllocOptions,
                                      NvU64 *pPages)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfacePmaPinPages(void *pPma,
                                    NvU64 *pPages,
                                    NvLength pageCount,
                                    NvU64 pageSize,
                                    NvU32 flags)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceMemoryFree(uvmGpuAddressSpaceHandle vaSpace,
                              UvmGpuPointer gpuPointer)
{
}

void nvUvmInterfacePmaFreePages(void *pPma,
                                NvU64 *pPages,
                                NvLength pageCount,
                                NvU64 pageSize,
                                NvU32 flags)
{
}

NV_STATUS nvUvmInterfaceMemoryCpuMap(uvmGpuAddressSpaceHandle vaSpace,
                                     UvmGpuPointer gpuPointer,
                                     NvLength length, void **cpuPtr,
                                     NvU64 pageSize)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceMemoryCpuUnMap(uvmGpuAddressSpaceHandle vaSpace,
                                  void *cpuPtr)
{
}

NV_STATUS nvUvmInterfaceTsgAllocate(uvmGpuAddressSpaceHandle vaSpace,
                                    const UvmGpuTsgAllocParams *allocParams,
                                    uvmGpuTsgHandle *tsg)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceTsgDestroy(uvmGpuTsgHandle tsg)
{
}

NV_STATUS nvUvmInterfaceChannelAllocate(const uvmGpuTsgHandle tsg,
                                        const UvmGpuChannelAllocParams *allocParams,
                                        uvmGpuChannelHandle *channel,
                                        UvmGpuChannelInfo *channelInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceChannelDestroy(uvmGpuChannelHandle channel)
{
}

NV_STATUS nvUvmInterfaceQueryCaps(uvmGpuDeviceHandle device,
                                  UvmGpuCaps *caps)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceQueryCopyEnginesCaps(uvmGpuDeviceHandle device,
                                             UvmGpuCopyEnginesCaps *caps)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceGetGpuInfo(const NvProcessorUuid *gpuUuid,
                                   const UvmGpuClientInfo *pGpuClientInfo,
                                   UvmGpuInfo *pGpuInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceServiceDeviceInterruptsRM(uvmGpuDeviceHandle device)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceSetPageDirectory(uvmGpuAddressSpaceHandle vaSpace,
                                         NvU64 physAddress, unsigned numEntries,
                                         NvBool bVidMemAperture, NvU32 pasid,
                                         NvU64 *dmaAddress)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceUnsetPageDirectory(uvmGpuAddressSpaceHandle vaSpace)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceDupAllocation(uvmGpuAddressSpaceHandle srcVaSpace,
                                      NvU64 srcAddress,
                                      uvmGpuAddressSpaceHandle dstVaSpace,
                                      NvU64 dstVaAlignment,
                                      NvU64 *dstAddress)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceDupMemory(uvmGpuDeviceHandle device,
                                  NvHandle hClient,
                                  NvHandle hPhysMemory,
                                  NvHandle *hDupMemory,
                                  UvmGpuMemoryInfo *pGpuMemoryInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceFreeDupedHandle(uvmGpuDeviceHandle device,
                                        NvHandle hPhysHandle)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceGetFbInfo(uvmGpuDeviceHandle device,
                                  UvmGpuFbInfo * fbInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceGetEccInfo(uvmGpuDeviceHandle device,
                                   UvmGpuEccInfo * eccInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceOwnPageFaultIntr(uvmGpuDeviceHandle device, NvBool bOwnInterrupts)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceInitFaultInfo(uvmGpuDeviceHandle device,
                                      UvmGpuFaultInfo *pFaultInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceDestroyFaultInfo(uvmGpuDeviceHandle device,
                                         UvmGpuFaultInfo *pFaultInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceHasPendingNonReplayableFaults(UvmGpuFaultInfo *pFaultInfo,
                                                      NvBool *hasPendingFaults)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceGetNonReplayableFaults(UvmGpuFaultInfo *pFaultInfo,
                                               void *pFaultBuffer,
                                               NvU32 *numFaults)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceFlushReplayableFaultBuffer(UvmGpuFaultInfo *pFaultInfo,
                                                   NvBool bCopyAndFlush)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceTogglePrefetchFaults(UvmGpuFaultInfo *pFaultInfo,
                                             NvBool bEnable)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceInitAccessCntrInfo(uvmGpuDeviceHandle device,
                                           UvmGpuAccessCntrInfo *pAccessCntrInfo,
                                           NvU32 accessCntrIndex)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceDestroyAccessCntrInfo(uvmGpuDeviceHandle device,
                                              UvmGpuAccessCntrInfo *pAccessCntrInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceEnableAccessCntr(uvmGpuDeviceHandle device,
                                         UvmGpuAccessCntrInfo *pAccessCntrInfo,
                                         const UvmGpuAccessCntrConfig *pAccessCntrConfig)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceDisableAccessCntr(uvmGpuDeviceHandle device,
                                          UvmGpuAccessCntrInfo *pAccessCntrInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceRegisterUvmCallbacks(struct UvmOpsUvmEvents *importedUvmOps)
{
    return NV_OK;
}

void nvUvmInterfaceDeRegisterUvmOps(void)
{
}

NV_STATUS nvUvmInterfaceGetNvlinkInfo(uvmGpuDeviceHandle device,
                                      UvmGpuNvlinkInfo *nvlinkInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceP2pObjectCreate(uvmGpuDeviceHandle device1,
                                        uvmGpuDeviceHandle device2,
                                        NvHandle *hP2pObject)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceP2pObjectDestroy(uvmGpuSessionHandle session,
                                    NvHandle hP2pObject)
{
}

NV_STATUS nvUvmInterfaceGetExternalAllocPtes(uvmGpuAddressSpaceHandle vaSpace,
                                             NvHandle hMemory,
                                             NvU64 offset,
                                             NvU64 size,
                                             UvmGpuExternalMappingInfo *gpuExternalMappingInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceGetExternalAllocPhysAddrs(uvmGpuAddressSpaceHandle vaSpace,
                                                  NvHandle hMemory,
                                                  NvU64 offset,
                                                  NvU64 size,
                                                  UvmGpuExternalPhysAddrInfo *gpuExternalPhysAddrsInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceRetainChannel(uvmGpuAddressSpaceHandle vaSpace,
                                      NvHandle hClient,
                                      NvHandle hChannel,
                                      void **retainedChannel,
                                      UvmGpuChannelInstanceInfo *channelInstanceInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceBindChannelResources(void *retainedChannel,
                                             UvmGpuChannelResourceBindParams *channelResourceBindParams)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceReleaseChannel(void *retainedChannel)
{
}

void nvUvmInterfaceStopChannel(void *retainedChannel, NvBool bImmediate)
{
}

NV_STATUS nvUvmInterfaceGetChannelResourcePtes(uvmGpuAddressSpaceHandle vaSpace,
                                               NvP64 resourceDescriptor,
                                               NvU64 offset,
                                               NvU64 size,
                                               UvmGpuExternalMappingInfo *externalMappingInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceReportNonReplayableFault(uvmGpuDeviceHandle device,
                                                 const void *pFaultPacket)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfacePagingChannelAllocate(uvmGpuDeviceHandle device,
                                              const UvmGpuPagingChannelAllocParams *allocParams,
                                              UvmGpuPagingChannelHandle *channel,
                                              UvmGpuPagingChannelInfo *channelInfo)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfacePagingChannelDestroy(UvmGpuPagingChannelHandle channel)
{
}

NV_STATUS nvUvmInterfacePagingChannelsMap(uvmGpuAddressSpaceHandle srcVaSpace,
                                          UvmGpuPointer srcAddress,
                                          uvmGpuDeviceHandle device,
                                          NvU64 *dstAddress)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfacePagingChannelsUnmap(uvmGpuAddressSpaceHandle srcVaSpace,
                                       UvmGpuPointer srcAddress,
                                       uvmGpuDeviceHandle device)
{
}

NV_STATUS nvUvmInterfacePagingChannelPushStream(UvmGpuPagingChannelHandle channel,
                                                char *methodStream,
                                                NvU32 methodStreamSize)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceReportFatalError(NV_STATUS error)
{
}

NV_STATUS nvUvmInterfaceCslInitContext(UvmCslContext *uvmCslContext,
                                       uvmGpuChannelHandle channel)
{
    return NV_ERR_NOT_SUPPORTED;
}

void nvUvmInterfaceDeinitCslContext(UvmCslContext *uvmCslContext)
{
}

NV_STATUS nvUvmInterfaceCslRotateKey(UvmCslContext *contextList[],
                                     NvU32 contextListCount)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceCslRotateIv(UvmCslContext *uvmCslContext,
                                    UvmCslOperation operation)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceCslEncrypt(UvmCslContext *uvmCslContext,
                                   NvU32 bufferSize,
                                   NvU8 const *inputBuffer,
                                   UvmCslIv *encryptIv,
                                   NvU8 *outputBuffer,
                                   NvU8 *authTagBuffer)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceCslDecrypt(UvmCslContext *uvmCslContext,
                                   NvU32 bufferSize,
                                   NvU8 const *inputBuffer,
                                   UvmCslIv const *decryptIv,
                                   NvU32 keyRotationId,
                                   NvU8 *outputBuffer,
                                   NvU8 const *addAuthData,
                                   NvU32 addAuthDataSize,
                                   NvU8 const *authTagBuffer)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceCslSign(UvmCslContext *uvmCslContext,
                                NvU32 bufferSize,
                                NvU8 const *inputBuffer,
                                NvU8 *authTagBuffer)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceCslQueryMessagePool(UvmCslContext *uvmCslContext,
                                            UvmCslOperation operation,
                                            NvU64 *messageNum)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceCslIncrementIv(UvmCslContext *uvmCslContext,
                                       UvmCslOperation operation,
                                       NvU64 increment,
                                       UvmCslIv *iv)
{
    return NV_ERR_NOT_SUPPORTED;
}

NV_STATUS nvUvmInterfaceCslLogEncryption(UvmCslContext *uvmCslContext,
                                         UvmCslOperation operation,
                                         NvU32 bufferSize)
{
    return NV_ERR_NOT_SUPPORTED;
}
