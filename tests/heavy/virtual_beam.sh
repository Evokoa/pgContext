#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
REPORT_PATH="${REPORT_PATH:-}"

virtual_beam_certification() {
    local output resource_report binary max_rss_bytes max_rss_ceiling
    local os_name kernel_release machine_arch cpu_count memory_bytes rustc_version
    resource_report="$(mktemp)"
    trap 'rm -f "${resource_report}"' RETURN
    (
        cd "${REPO_ROOT}"
        cargo build --release -p context-test \
            --bin p16_virtual_beam_certification \
            --bin p16_virtual_beam_manifest
    )
    binary="${REPO_ROOT}/target/release/p16_virtual_beam_certification"
    if [[ "$(uname -s)" == "Darwin" ]]; then
        output="$(/usr/bin/time -l "${binary}" 2>"${resource_report}")"
        max_rss_bytes="$(awk '/maximum resident set size/{print $1; exit}' "${resource_report}")"
    else
        output="$(/usr/bin/time -v "${binary}" 2>"${resource_report}")"
        max_rss_bytes="$(awk -F: '/Maximum resident set size/{gsub(/[[:space:]]/, "", $2); print $2 * 1024; exit}' "${resource_report}")"
    fi
    max_rss_ceiling="$("${REPO_ROOT}/target/release/p16_virtual_beam_manifest" \
        | awk -F '\t' '$1 == "max_rss_bytes" {print $2}')"
    [[ "${max_rss_bytes}" =~ ^[0-9]+$ ]]
    [[ "${max_rss_ceiling}" =~ ^[0-9]+$ ]]
    (( max_rss_bytes <= max_rss_ceiling ))
    output+=$'\n'"virtual_beam_rss"$'\t'"max_rss_bytes=${max_rss_bytes} ceiling_bytes=${max_rss_ceiling}"
    os_name="$(uname -s)"
    kernel_release="$(uname -r)"
    machine_arch="$(uname -m)"
    cpu_count="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf 'unknown')"
    if [[ "${os_name}" == "Darwin" ]]; then
        memory_bytes="$(sysctl -n hw.memsize 2>/dev/null || printf 'unknown')"
    else
        memory_bytes="$(awk '/^MemTotal:/{print $2 * 1024; exit}' /proc/meminfo 2>/dev/null || printf 'unknown')"
    fi
    rustc_version="$(rustc --version | tr ' ' '_')"
    output+=$'\n'"virtual_beam_environment"$'\t'"os=${os_name} kernel=${kernel_release} arch=${machine_arch} cpu_count=${cpu_count} memory_bytes=${memory_bytes} rustc=${rustc_version}"
    while IFS= read -r marker; do
        grep -Fq -- "${marker}" <<<"${output}"
    done < <(
        "${REPO_ROOT}/target/release/p16_virtual_beam_manifest" \
            | awk -F '\t' '$1 == "report_markers" {print $2}' \
            | tr ',' '\n'
    )
    grep -Fq -- $'virtual_beam_decision\tdecision=pass' <<<"${output}"
    if [[ -n "${REPORT_PATH}" ]]; then
        mkdir -p "$(dirname "${REPORT_PATH}")"
        printf '%s\n' "${output}" >"${REPORT_PATH}"
    fi
    printf '%s\n' "${output}"
}

virtual_beam_certification
