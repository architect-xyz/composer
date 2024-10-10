# composer

Run or restart docker-compose services on a cron schedule.  This composer can itself be run as a docker-compose service--see `compose.yml` for an example.

Compared to `ofelia` and `reddec/compose-scheduler`, the novel approach taken here is to leverage the Docker CLI itself to parse a compose configuration.  This allows us to use the simple labeling scheme without the shortcomings of only liaising with the Docker daemon.  This allows us to pick up compose file changes on the fly, run scheduled tasks that haven't been run for a first time, and restart compose services as if the host itself were restarting them. 