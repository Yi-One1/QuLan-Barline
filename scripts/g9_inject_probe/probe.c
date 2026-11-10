/* G9 phase-1 minimal read-only probe DLL.
 *
 * Scope (strictly read-only):
 *   - DllMain is intentionally empty: no COM init, no library loading,
 *     no thread creation, no registry access, no system state changes.
 *   - The single exported entry point performs in-process read-only
 *     enumeration (taskbar window tree, runtime module mapping, process id)
 *     and writes the result to a caller supplied UTF-16 path as UTF-8 text.
 *
 * Explicitly NOT DONE here (all forbidden by the experiment red lines):
 *   no hook installation, no window procedure replacement, no color writes,
 *   no registry writes, no composition / DWM attribute writes, no parent
 *   window changes, no process termination, no resource replacement,
 *   no writes to other processes, no changes to other windows.
 *
 * Because no window procedure ever points into this DLL, the host can
 * release it after the remote thread returns (see g9_inject_probe.py).
 *
 * Build: see build.ps1 (MSVC cl.exe /LD). Zero third-party dependencies.
 * The engineer round does NOT load this DLL into Explorer; phase-1 live
 * execution is operator-only.
 */

#include <windows.h>
#include <stdio.h>

#define OUT_BUF_WCHARS 2048
#define UTF8_BUF_BYTES 8192

static HANDLE g_out = INVALID_HANDLE_VALUE;

/* Convert one wide line to UTF-8 and append it to the result file. */
static void emit_utf8(const wchar_t *text) {
    char buf[UTF8_BUF_BYTES];
    int n = 0;
    DWORD written = 0;

    if (g_out == INVALID_HANDLE_VALUE || text == NULL) {
        return;
    }
    n = WideCharToMultiByte(CP_UTF8, 0, text, -1, buf,
                            (int)sizeof(buf), NULL, NULL);
    if (n <= 1) {
        return;
    }
    WriteFile(g_out, buf, (DWORD)(n - 1), &written, NULL);
}

/* Keep one node on one line: strip CR/LF/TAB from window titles. */
static void sanitize(wchar_t *s) {
    wchar_t *p = s;
    if (s == NULL) {
        return;
    }
    for (; *p != L'\0'; ++p) {
        if (*p == L'\r' || *p == L'\n' || *p == L'\t') {
            *p = L' ';
        }
    }
}

/* Read-only callback: record class / title / visibility / rect / pid. */
static BOOL CALLBACK enum_child_cb(HWND hwnd, LPARAM unused) {
    wchar_t cls[256] = {0};
    wchar_t title[512] = {0};
    wchar_t line[OUT_BUF_WCHARS] = {0};
    RECT r = {0, 0, 0, 0};
    DWORD pid = 0;

    (void)unused;
    GetClassNameW(hwnd, cls, 256);
    GetWindowTextW(hwnd, title, 512);
    sanitize(title);
    GetWindowRect(hwnd, &r);
    GetWindowThreadProcessId(hwnd, &pid);

    swprintf_s(line, OUT_BUF_WCHARS,
               L"node hwnd=0x%p class=%s title=%s visible=%d "
               L"rect=%ld,%ld,%ld,%ld pid=%lu\r\n",
               (void *)hwnd, cls, title,
               IsWindowVisible(hwnd) ? 1 : 0,
               (long)r.left, (long)r.top, (long)r.right, (long)r.bottom,
               (unsigned long)pid);
    emit_utf8(line);
    return TRUE;
}

BOOL APIENTRY DllMain(HMODULE module, DWORD reason, LPVOID reserved) {
    (void)module;
    (void)reason;
    (void)reserved;
    return TRUE; /* intentionally empty */
}

/* Exported entry point used as the remote thread start address.
 * param = pointer to a wide string with the result file path (written into
 * the target process by the host via process memory write).
 * Returns 0 on success, 2 = missing path, 3 = result file open failed.
 */
__declspec(dllexport) DWORD WINAPI G9ReadOnlyProbe(LPVOID param) {
    const wchar_t *path = (const wchar_t *)param;
    HWND root = NULL;
    wchar_t line[OUT_BUF_WCHARS] = {0};
    DWORD self_pid = GetCurrentProcessId();
    size_t i = 0;

    static const wchar_t *kModules[] = {
        L"Windows.UI.Xaml.dll",
        L"UIAutomationCore.dll",
        L"dcomp.dll",
        L"dwmapi.dll",
        L"Windows.UI.dll"
    };

    if (path == NULL) {
        return 2;
    }
    g_out = CreateFileW(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                        FILE_ATTRIBUTE_NORMAL, NULL);
    if (g_out == INVALID_HANDLE_VALUE) {
        return 3;
    }

    emit_utf8(L"G9PROBE/1.0\r\n");
    swprintf_s(line, OUT_BUF_WCHARS, L"probe_pid=%lu\r\n",
               (unsigned long)self_pid);
    emit_utf8(line);

    root = FindWindowW(L"Shell_TrayWnd", NULL);
    swprintf_s(line, OUT_BUF_WCHARS,
               L"tray_root=0x%p found=%d\r\n", (void *)root, root ? 1 : 0);
    emit_utf8(line);
    if (root != NULL) {
        EnumChildWindows(root, enum_child_cb, 0);
    }

    /* Read-only reachability signal: check whether runtime modules are
     * already mapped in this process. Module presence alone does NOT prove
     * any interface is usable; this is recorded as an indirect signal only.
     */
    for (i = 0; i < sizeof(kModules) / sizeof(kModules[0]); ++i) {
        HMODULE mod = GetModuleHandleW(kModules[i]);
        swprintf_s(line, OUT_BUF_WCHARS,
                   L"module name=%s mapped=%d\r\n",
                   kModules[i], mod ? 1 : 0);
        emit_utf8(line);
    }

    emit_utf8(L"note=in-process read-only enumeration only: window tree, "
              L"runtime module mapping, process id. No interface-level "
              L"diagnostics, no automation tree walk, no hooks, no window "
              L"procedure replacement, no color writes, no registry writes, "
              L"no composition writes, no process termination, no changes "
              L"to other windows.\r\n");
    emit_utf8(L"end=ok\r\n");

    CloseHandle(g_out);
    g_out = INVALID_HANDLE_VALUE;
    return 0;
}
