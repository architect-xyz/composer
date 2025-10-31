alias docker="echo \"Use <logs|start|stop|restart|down|up|upgrade|status> instead.\""

logs() {
    /usr/bin/docker compose logs "$1" -n 100 -f
}

start() {
    /usr/bin/docker compose start "$1"
}

stop() {
    if [ -z "$1" ]; then
        echo "Usage: stop <service>"
        return 1
    fi
    echo -e "\e[30;103m[ WARN ]\e[0m This will stop $1."
    read -p "Continue? [y/N] " choice
    choice=${choice:-N}
    if [[ $choice =~ ^[Yy]$ ]]; then
        /usr/bin/docker compose stop "$1"
    else
        echo "User aborted."
    fi
}

restart() {
    /usr/bin/docker compose restart "$1"
}

down() {
    if [ -z "$1" ]; then
        echo "Usage: down <service>"
        return 1
    fi
    echo -e "\e[30;103m[ WARN ]\e[0m This will stop $1 and destroy its container."
    echo -e "\e[30;103m[ WARN ]\e[0m Use start/stop instead if this isn't the desired behavior." 
    read -p "Continue? [y/N] " choice
    choice=${choice:-N}
    if [[ $choice =~ ^[Yy]$ ]]; then
        /usr/bin/docker compose down "$1"
    else
        echo "User aborted."
    fi
}

up() {
    if [ -z "$1" ]; then
        echo "Usage: up <service>"
        return 1
    fi
    echo -e "\e[30;103m[ WARN ]\e[0m This will recreate and upgrade $1 if there are any pending image updates."
    echo -e "\e[30;103m[ WARN ]\e[0m Use start/stop instead if this isn't the desired behavior." 
    read -p "Continue? [y/N] " choice
    choice=${choice:-N}
    if [[ $choice =~ ^[Yy]$ ]]; then
        /usr/bin/docker compose up -d "$1"
    else
        echo "User aborted."
    fi
}

upgrade() {
    if [ -z "$1" ]; then
        echo "Usage: upgrade <service|all>"
        return 1
    fi

    if [ "$1" = "all" ]; then
        echo -e "\e[30;103m[ WARN ]\e[0m This will pull latest images and upgrade ALL services."
        read -p "Continue? [y/N] " choice
        choice=${choice:-N}
        if [[ $choice =~ ^[Yy]$ ]]; then
            /usr/bin/docker compose pull
            /usr/bin/docker compose up -d
        else
            echo "User aborted."
        fi
    else
        echo -e "\e[30;103m[ WARN ]\e[0m This will pull latest image and upgrade $1."
        read -p "Continue? [y/N] " choice 
        choice=${choice:-N}
        if [[ $choice =~ ^[Yy]$ ]]; then
            /usr/bin/docker compose pull "$1"
            /usr/bin/docker compose up -d "$1"
        else
            echo "User aborted."
        fi
    fi
}

status() {
    if [ -z "$1" ]; then
        /usr/bin/docker run --rm -it afintech/composer:latest status
    else
        /usr/bin/docker compose ps "$1"
    fi
}

exec() {
    if [ -z "$1" ]; then
        echo "Usage: exec <service> [command]"
        return 1
    fi
    /usr/bin/docker compose exec "$1" "${@:2}"
}
