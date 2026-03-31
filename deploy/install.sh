#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# deploy/install.sh
# Installs polycopier as a systemd service on a Linux server.
#
# Usage (run as root or with sudo):
#   chmod +x deploy/install.sh
#   sudo ./deploy/install.sh /path/to/polycopier-binary
#
# After first install, configure the bot:
#   sudo -u polycopier /opt/polycopier/polycopier  ← runs interactive wizard
#   (Ctrl-C after .env is written, then start the service normally)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

BINARY="${1:-./target/release/polycopier}"
INSTALL_DIR="/opt/polycopier"
SERVICE_FILE="deploy/polycopier.service"
SYSTEM_USER="polycopier"

if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: This script must be run as root (sudo ./deploy/install.sh)" >&2
  exit 1
fi

echo "==> Creating system user '${SYSTEM_USER}' (if not exists)..."
id -u "${SYSTEM_USER}" &>/dev/null || \
  useradd --system --shell /bin/false --home-dir "${INSTALL_DIR}" --create-home "${SYSTEM_USER}"

echo "==> Creating install directory ${INSTALL_DIR}..."
mkdir -p "${INSTALL_DIR}"
chown "${SYSTEM_USER}:${SYSTEM_USER}" "${INSTALL_DIR}"

echo "==> Copying binary..."
cp -f "${BINARY}" "${INSTALL_DIR}/polycopier"
chmod 755 "${INSTALL_DIR}/polycopier"
chown "${SYSTEM_USER}:${SYSTEM_USER}" "${INSTALL_DIR}/polycopier"

echo "==> Installing systemd unit..."
cp -f "${SERVICE_FILE}" /etc/systemd/system/polycopier.service
systemctl daemon-reload
systemctl enable polycopier.service

echo ""
echo "✓ Installation complete."
echo ""
echo "NEXT STEPS:"
echo "  1. Configure the bot (first-time .env setup):"
echo "     sudo -u ${SYSTEM_USER} ${INSTALL_DIR}/polycopier"
echo "     # Follow the interactive wizard, then Ctrl-C"
echo ""
echo "  2. Start the service:"
echo "     sudo systemctl start polycopier"
echo ""
echo "  3. Check status / logs:"
echo "     sudo systemctl status polycopier"
echo "     sudo journalctl -u polycopier -f"
echo ""
echo "  4. Update binary in future:"
echo "     sudo systemctl stop polycopier"
echo "     sudo cp ./target/release/polycopier ${INSTALL_DIR}/polycopier"
echo "     sudo systemctl start polycopier"
