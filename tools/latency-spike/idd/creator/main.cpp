/*++

Copyright (c) Microsoft Corporation

Derived from microsoft/Windows-driver-samples@717778a2 (MIT) for the mdrdp latency spike.

Abstract:

    Creates the software device that makes Windows load mdrdp_idd.dll, then holds it
    open. The virtual display exists for exactly as long as this process lives:
    the SwDevice handle is owned by the process, so killing the process (or Ctrl+C
    under --wait) destroys it and unplugs the display.

    Usage:
        mdrdp-idd-create.exe            create the device, then block forever.
        mdrdp-idd-create.exe --wait     same, but Ctrl+C closes the device cleanly first.

    The sample's interactive "press x to exit" loop is gone - it is useless over SSH,
    which is how this runs on the test host.

--*/

#include <stdio.h>

#include <windows.h>
#include <swdevice.h>

static HANDLE g_hExitEvent = nullptr;

VOID WINAPI
CreationCallback(
    _In_ HSWDEVICE hSwDevice,
    _In_ HRESULT hrCreateResult,
    _In_opt_ PVOID pContext,
    _In_opt_ PCWSTR pszDeviceInstanceId
    )
{
    HANDLE hEvent = *(HANDLE*) pContext;

    if (SUCCEEDED(hrCreateResult) && pszDeviceInstanceId != nullptr)
    {
        wprintf(L"Device instance id: %s\n", pszDeviceInstanceId);
    }

    SetEvent(hEvent);
    UNREFERENCED_PARAMETER(hSwDevice);
}

static BOOL WINAPI CtrlHandler(DWORD CtrlType)
{
    switch (CtrlType)
    {
    case CTRL_C_EVENT:
    case CTRL_BREAK_EVENT:
    case CTRL_CLOSE_EVENT:
    case CTRL_LOGOFF_EVENT:
    case CTRL_SHUTDOWN_EVENT:
        if (g_hExitEvent != nullptr)
        {
            SetEvent(g_hExitEvent);
        }
        return TRUE;
    default:
        return FALSE;
    }
}

int __cdecl wmain(int argc, wchar_t *argv[])
{
    bool bWaitForCtrlC = false;
    for (int i = 1; i < argc; i++)
    {
        if (_wcsicmp(argv[i], L"--wait") == 0)
        {
            bWaitForCtrlC = true;
        }
    }

    HANDLE hEvent = CreateEvent(nullptr, FALSE, FALSE, nullptr);
    HSWDEVICE hSwDevice;
    SW_DEVICE_CREATE_INFO createInfo = { 0 };
    PCWSTR description = L"mdrdp latency-spike display";

    // These match the PnP ids in mdrdp-idd.inf so the OS loads the driver when the
    // device is created.
    PCWSTR instanceId = L"mdrdp_idd";
    PCWSTR hardwareIds = L"mdrdp_idd\0\0";
    PCWSTR compatibleIds = L"mdrdp_idd\0\0";

    createInfo.cbSize = sizeof(createInfo);
    createInfo.pszzCompatibleIds = compatibleIds;
    createInfo.pszInstanceId = instanceId;
    createInfo.pszzHardwareIds = hardwareIds;
    createInfo.pszDeviceDescription = description;

    createInfo.CapabilityFlags = SWDeviceCapabilitiesRemovable |
                                 SWDeviceCapabilitiesSilentInstall |
                                 SWDeviceCapabilitiesDriverRequired;

    // Create the device
    HRESULT hr = SwDeviceCreate(L"mdrdp_idd",
                                L"HTREE\\ROOT\\0",
                                &createInfo,
                                0,
                                nullptr,
                                CreationCallback,
                                &hEvent,
                                &hSwDevice);
    if (FAILED(hr))
    {
        printf("SwDeviceCreate failed with 0x%lx\n", hr);
        return 1;
    }

    // Wait for callback to signal that the device has been created
    printf("Waiting for device to be created....\n");
    DWORD waitResult = WaitForSingleObject(hEvent, 10*1000);
    if (waitResult != WAIT_OBJECT_0)
    {
        printf("Wait for device creation failed\n");
        return 1;
    }
    printf("Device created\n\n");

    g_hExitEvent = CreateEvent(nullptr, TRUE, FALSE, nullptr);
    if (g_hExitEvent == nullptr)
    {
        printf("CreateEvent for the exit event failed with %lu\n", GetLastError());
        SwDeviceClose(hSwDevice);
        return 1;
    }

    if (bWaitForCtrlC)
    {
        if (!SetConsoleCtrlHandler(CtrlHandler, TRUE))
        {
            printf("SetConsoleCtrlHandler failed with %lu\n", GetLastError());
            SwDeviceClose(hSwDevice);
            return 1;
        }
        printf("Holding the display open. Press Ctrl+C to unplug it and exit.\n");
    }
    else
    {
        // No handler installed: this event is never signalled, so the process blocks
        // until it is killed. Killing it destroys the SwDevice and unplugs the display.
        printf("Holding the display open. Kill this process to unplug it.\n");
    }
    fflush(stdout);

    WaitForSingleObject(g_hExitEvent, INFINITE);

    // Stop the device; this causes the driver to be unloaded.
    SwDeviceClose(hSwDevice);

    return 0;
}
