# Standalone LAN connections

Android and iPhone/iPad controllers can connect directly to a Windows host on the same local network. Relay failures and later relay registrations do not end these sessions.

1. Update the Windows installation and the mobile app, and keep Airhand running on Windows.
2. Open **LAN connection (no relay needed)** in the Windows tray and click the current network adapter's address to copy it, for example `192.168.1.20:4434`.
3. Select **LAN connection** on the mobile Remote page, enter that address, and connect.
4. Enter the Windows tray's pairing code when requested. Allow local network access on iPad.

The device remembers its local address and verified identity. LAN and relay reconnect tokens are saved separately.

The listener uses **UDP 4434**. The all-users Windows installer allows this port for the local subnet on private networks. Keep the trusted local network set to Private; a per-user installation may require allowing the app in the Windows firewall prompt. Both devices must be reachable on that network, and the host must be awake with Airhand running.
