; Firewall rule for KVM Bridge. Other computers find this one over UDP
; (port 47800) and connect to it over TCP (port 47801). The rule lets them
; reach KVM Bridge itself, and only from the local network.

!macro KVMB_NETSH ARGS
  Push $0
  nsExec::Exec 'netsh advfirewall firewall ${ARGS}'
  Pop $0
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro KVMB_NETSH 'delete rule name="KVM Bridge"'
  !insertmacro KVMB_NETSH 'add rule name="KVM Bridge" dir=in action=allow program="$INSTDIR\kvmbridge.exe" remoteip=localsubnet profile=any enable=yes'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro KVMB_NETSH 'delete rule name="KVM Bridge"'
!macroend
