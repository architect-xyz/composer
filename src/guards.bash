# Composer guards (installed via `composer install <shell> --with-guards`).
# Shadows `docker` to steer users at the wrapper commands and enables
# interactive confirmation prompts on destructive operations.

export COMPOSER_GUARDS=1

alias docker="echo 'docker is shadowed by composer guards — use <status|logs|start|stop|restart|up|down|upgrade|run|exec> instead (or unset COMPOSER_GUARDS and unalias docker).'"
