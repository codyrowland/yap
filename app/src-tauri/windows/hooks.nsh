; Yap (whisper.cpp) and the clean-up helper (llama-server) need the Visual
; C++ runtime, at least the version Yap was built with (14.50). If it is
; missing or older, the bundled vc_redist.x64.exe installs it; Windows asks
; for administrator rights once.
!define YAP_VC_MINOR 50
!define YAP_HOOKS_DIR "${__FILEDIR__}"

!macro NSIS_HOOK_POSTINSTALL
  SetRegView 64
  ReadRegDWORD $0 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" "Installed"
  ReadRegDWORD $1 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" "Minor"
  SetRegView default
  ${If} $0 != 1
  ${OrIf} $1 < ${YAP_VC_MINOR}
    DetailPrint "Installing the Microsoft Visual C++ runtime..."
    InitPluginsDir
    File "/oname=$PLUGINSDIR\vc_redist.x64.exe" "${YAP_HOOKS_DIR}\vc_redist.x64.exe"
    ExecShellWait "" "$PLUGINSDIR\vc_redist.x64.exe" "/install /quiet /norestart" SW_HIDE
    Delete "$PLUGINSDIR\vc_redist.x64.exe"
  ${EndIf}
!macroend
