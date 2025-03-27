#!/bin/sh

# Randomly exit with error code 1 (approximately 50% of the time)
if [ $((RANDOM % 2)) -eq 0 ]; then
  echo "Hello world!"
  exit 0
else
  echo "Hello world!"
  exit 1
fi