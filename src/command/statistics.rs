use std::fmt;

use sqlx::Row;

use super::*;

struct JoinReason {
    hangout: u32,
    help: u32,
    develop: u32,
}

struct FoundFrom {
    friend: u32,
    search_engine: u32,
    youtube: u32,
    twitter: u32,
    market_cap: u32,
    meetup: u32,
}

impl fmt::Display for JoinReason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "hangout: {}\nhelp: {}\ndevelop: {}\n",
            self.hangout, self.help, self.develop
        )
    }
}

impl fmt::Display for FoundFrom {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Friend: {}\nSearch Engine: {}\nYouTube: {}\nTwitter: {}\nMarket Cap: {}\nMeetup: {}\n",
            self.friend,
            self.search_engine,
            self.youtube,
            self.twitter,
            self.market_cap,
            self.meetup
        )
    }
}

impl Db {
    async fn get_join_reason_stats(&self) -> Result<JoinReason> {
        let q = sqlx::query("select * from join_reason")
            .fetch_one(&self.sqlitedb)
            .await?;

        Ok(JoinReason {
            hangout: q.get("hangout"),
            help: q.get("help"),
            develop: q.get("develop"),
        })
    }

    async fn get_found_from_stats(&self) -> Result<FoundFrom> {
        let q = sqlx::query("select * from found_from")
            .fetch_one(&self.sqlitedb)
            .await?;

        Ok(FoundFrom {
            friend: q.get("friend"),
            search_engine: q.get("search_engine"),
            youtube: q.get("youtube"),
            twitter: q.get("twitter"),
            market_cap: q.get("market_cap"),
            meetup: q.get("meetup"),
        })
    }
}

#[command]
#[required_permissions(ADMINISTRATOR)]
async fn statistics(ctx: &Context, msg: &Message, _args: Args) -> CommandResult {
    //println!("{:?}", ctx);
    match _args.message() {
        "join_reason" => {
            let join_reason = ctx.get_db().await.get_join_reason_stats().await.unwrap();
            let reply = format!("{}", join_reason);
            msg.reply(&ctx.http, reply).await?;
        }
        "found_from" => {
            let found_from = ctx.get_db().await.get_found_from_stats().await.unwrap();
            let reply = format!("{}", found_from);
            msg.reply(&ctx.http, reply).await?;
        }
        _ => {
            msg.reply(&ctx.http, "Not a valid query.\n Valid statistics to look for are `statistics join_reason` and `statistics found_from`").await?;
        }
    }
    Ok(())
}
