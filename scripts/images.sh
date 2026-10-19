#!/usr/bin/env bash
# Shared argument and tag validation; sourcing this file does not run Podman.
mneme_select_services() {
    mneme_quadlet_dir="${MNEME_QUADLET_DIR:-$HOME/.config/containers/systemd}"
    mneme_systemd_dir="${MNEME_SYSTEMD_DIR:-$HOME/.config/systemd/user}"
    mneme_services=()
    local mneme_service mneme_existing
    if [ "$#" = 0 ]; then set -- api models tools; fi
    for mneme_service in "$@"; do
        case "$mneme_service" in api|models|tools) ;; *) echo '服务只能是 api、models 或 tools。' >&2; return 1 ;; esac
        mneme_existing=" ${mneme_services[*]} "
        if [[ "$mneme_existing" != *" $mneme_service "* ]]; then mneme_services+=("$mneme_service"); fi
    done
    mneme_image_tag="${MNEME_IMAGE_TAG:-latest}"
    if [[ ! "$mneme_image_tag" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
        echo 'MNEME_IMAGE_TAG 必须是合法的容器镜像标签。' >&2; return 1
    fi
}
