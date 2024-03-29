# bamboo-leave-to-slack-bot

## About this bot / why does this exist?

BambooHR has a Slack plugin, but it's pretty basic and has some shortcomings. This little utility fills some gaps.

- Posts to a channel for quick reference, rather than requiring users to interact with a bot (minor, but it's a small quality of life improvement).

- Shows holidays, which is useful in a team that spans multiple countries with holidays you might not be aware of.

- Merges consecutive periods of leave, to try and better show when the person on leave will return.

### Known shortcomings

- I've assumed that leave either side of a weekend (Saturday and Sunday) can be merged. I know this isn't the case for all people and places, but it meets my needs.

- I'm pretty liberal with `.unwrap()` on date manipulations. I feel like if you've managed to get leave approved on a date that can't be represented, you've probably got more interesting issues to deal with. 

- We could be smarter about leave that's scheduled starting Monday. I feel like we should be reporting the person as out starting on Saturday, for clarity.

## Setup and configuration

- Download or build a binary (TODO: set up GitHub actions to publish automatically.)

- Provide three environment variables:
    - `BAMBOO_COMPANY_DOMAIN`: company domain for your BambooHR instance.
    - `BAMBOO_API_KEY`: API key of a BambooHR user with sufficient privileges to download who's out.
    - `SLACK_WEBHOOK_URL`: URL of a Slack "Incoming Webhook" integration.

- Run the bot periodically. Cron is likely the easiest way, but you're free to choose your own adventure here.

### BambooHR API key permissions

If the BambooHR user you use to pull leave information has sufficient permissions, this bot will fetch and display the preferred name for users, rather than the name provided by the leave endpoint. This isn't well tested at this point in time, so your mileage may vary.
