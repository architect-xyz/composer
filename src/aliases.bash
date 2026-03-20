# Canonical composer shell aliases.
# Installed by `composer install bash` or `composer install zsh`.
# Works in both bash and zsh.
#
# If COMPOSE_PROJECT_DIR is set (e.g. by a project's subshell-env script),
# all docker compose commands use --project-directory. Otherwise, docker
# compose runs in the current working directory.

_dc() {
    docker compose ${COMPOSE_PROJECT_DIR:+--project-directory "$COMPOSE_PROJECT_DIR"} "$@"
}

# Extract --profile flags from args. Sets _PROFILE and _ARGS arrays.
_parse_flags() {
    _PROFILE=()
    _ARGS=()
    while [ $# -gt 0 ]; do
        case "$1" in
            --profile) _PROFILE=(--profile "$2"); shift 2 ;;
            *) _ARGS+=("$1"); shift ;;
        esac
    done
}

status() {
    if [ -z "$1" ]; then
        curl -sf http://localhost:10080/status.txt 2>/dev/null && return 0
        echo "composer status endpoint not available"
        return 1
    else
        _dc ps "$1"
    fi
}

logs() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"
    local lines=100
    local svc=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -n) lines="$2"; shift 2 ;;
            *) svc="$1"; shift ;;
        esac
    done
    if [ -z "$svc" ]; then
        echo "Usage: logs [-n lines] <service>"
        return 1
    fi
    _dc "${_PROFILE[@]}" logs "$svc" -n "$lines" -f
}

start() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"
    if [ -z "$1" ]; then
        echo "Usage: start <service>"
        return 1
    fi
    _dc "${_PROFILE[@]}" start "$1"
}

stop() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"
    if [ -z "$1" ]; then
        echo "Usage: stop <service>"
        return 1
    fi
    _dc "${_PROFILE[@]}" stop "$1"
}

restart() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"
    if [ -z "$1" ]; then
        echo "Usage: restart <service>"
        return 1
    fi
    _dc "${_PROFILE[@]}" restart "$1"
}

up() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"

    local all=false
    local services=()
    while [ $# -gt 0 ]; do
        case "$1" in
            -a|--all) all=true; shift ;;
            *) services+=("$1"); shift ;;
        esac
    done

    if [ "$all" = true ]; then
        echo -e "\e[30;103m[ WARN ]\e[0m This will recreate and upgrade all services if there are any pending image updates."
        _dc "${_PROFILE[@]}" up -d
    elif [ ${#services[@]} -gt 0 ]; then
        echo -e "\e[30;103m[ WARN ]\e[0m This will recreate and upgrade ${services[*]} if there are any pending image updates."
        echo -e "\e[30;103m[ WARN ]\e[0m Use start/stop instead if this isn't the desired behavior."
        _dc "${_PROFILE[@]}" up --remove-orphans -d "${services[@]}"
    else
        echo "Usage: up [--profile <name>] [-a] <service...>"
        return 1
    fi
}

down() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"

    local all=false
    local volume_flag=""
    local services=()
    while [ $# -gt 0 ]; do
        case "$1" in
            -a|--all) all=true; shift ;;
            -v) volume_flag="-v"; shift ;;
            *) services+=("$1"); shift ;;
        esac
    done

    if [ "$all" = true ]; then
        echo -e "\e[30;103m[ WARN ]\e[0m This will stop all services and destroy their containers."
        _dc "${_PROFILE[@]}" down --remove-orphans $volume_flag
    elif [ ${#services[@]} -gt 0 ]; then
        echo -e "\e[30;103m[ WARN ]\e[0m This will stop ${services[*]} and destroy their containers."
        echo -e "\e[30;103m[ WARN ]\e[0m Use start/stop instead if this isn't the desired behavior."
        _dc "${_PROFILE[@]}" down $volume_flag "${services[@]}"
    else
        echo "Usage: down [--profile <name>] [-a] [-v] <service...>"
        return 1
    fi
}

upgrade() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"

    local now=false
    local target=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --now) now=true; shift ;;
            *) target="$1"; shift ;;
        esac
    done

    if [ -z "$target" ]; then
        echo "Usage: upgrade [--profile <name>] [--now] <service>"
        return 1
    fi

    _dc "${_PROFILE[@]}" pull "$target"
    if [ "$now" = true ]; then
        _dc "${_PROFILE[@]}" down "$target"
        _dc "${_PROFILE[@]}" up --remove-orphans -d "$target"
    fi
}

run() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"
    if [ -z "$1" ]; then
        echo "Usage: run [--profile <name>] <service> [args...]"
        return 1
    fi
    local svc="$1"; shift
    _dc "${_PROFILE[@]}" run --rm "$svc" "$@"
}

exec() {
    _parse_flags "$@"
    set -- "${_ARGS[@]}"
    if [ -z "$1" ]; then
        echo "Usage: exec <service> [args...]"
        return 1
    fi
    local svc="$1"; shift
    _dc "${_PROFILE[@]}" exec "$svc" "$@"
}

# Print help on source
printf '\nAvailable commands:\n'
printf '  \033[32mInspect ──\033[0m status [svc] \033[32m·\033[0m logs [-n lines] <svc>\n'
printf '  \033[32mControl ──\033[0m start|stop|restart <svc>\n'
printf '  \033[32mDeploy  ──\033[0m up|down [-a] [-v] <svc...> \033[32m·\033[0m upgrade [--now] <svc>\n'
printf '  \033[32mExec    ──\033[0m run <svc> [args] \033[32m·\033[0m exec <svc> [args]\n'
printf '  \033[90mAll commands accept --profile <name>\033[0m\n\n'
