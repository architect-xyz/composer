#!/bin/sh

# wait for 5 seconds to make docker compose logs -f look right;
# if we start right away we usually miss the first few seconds
# as the restarted container isn't attached to immediately
sleep 5

echo "Today is $(date), and MY_ENV_VAR = $MY_ENV_VAR"
while true;
do
    echo "Beep";
    sleep 1;
    echo "Boop";
    sleep 1;
done
