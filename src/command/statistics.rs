use std::fmt;

use sqlx::Row;

use super::*;

struct JoinReason {
    hangout: u32,
    help: u32,
    develop: u32,
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
        _ => println!("No valid argument supplied"),
    }
    Ok(())
}
