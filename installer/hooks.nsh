; Integrand NSIS installer hooks (see tauri.conf.json > bundle.windows.nsis.installerHooks).
;
; The installer follows the usual three stages:
;   pre-installation   license page, then the checks below (Windows version, running copy)
;   installation       copies the app (~10 MB) for the current user, no admin rights
;   post-installation  shortcuts, uninstaller entry, and the finish page, which launches
;                      Integrand straight into engine setup
;
; The recognition engine (private Python, PyTorch, model; 2-5 GB depending on the GPU) is set up
; by the app itself, with its own before/install/finish phases, GPU detection, progress and retry.
!include WinVer.nsh

!define MUI_FINISHPAGE_RUN_TEXT "Launch Integrand and set up the recognition engine"

!macro NSIS_HOOK_PREINSTALL
  ${IfNot} ${AtLeastWin10}
    MessageBox MB_ICONSTOP|MB_OK "Integrand needs Windows 10 or 11."
    Abort
  ${EndIf}
  ; An open copy holds integrand.exe and its engine (python.exe) open; close it so files can be replaced.
  nsExec::Exec 'taskkill /F /T /IM integrand.exe'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; The engine runs as python.exe inside the app's data folder; stop it so "Delete app data"
  ; can remove the folder. (It normally exits with the app, which the uninstaller closes first.)
  nsExec::Exec 'taskkill /F /T /IM integrand.exe'
!macroend
