use std::collections::HashMap;

use super::*;
use crate::db::{ClientContextExt, Db};
use substr::StringUtils;

use meilisearch_sdk::{client::Client as MeiliClient, settings::Settings, errors::Error};
use serde::{Deserialize, Serialize};

use serenity::{
    futures::StreamExt,
    // http::AttachmentType,
    model::{
        self,
        application::interaction::{message_component::MessageComponentInteraction, MessageFlags},
        channel::{AttachmentType, Embed},
        guild::{Emoji, Role},
        id::RoleId,
        prelude::component::Button,
        Permissions,
    },
    utils::{read_image, MessageBuilder},
};

use urlencoding::encode;

impl Db {
    pub async fn increment_join_reason(&self, data_name: &str) -> Result<()> {
        let q = format!("update join_reason set {} = {} + 1", data_name, data_name);
        sqlx::query(&q).execute(&self.sqlitedb).await?;
        Ok(())
    }

    pub async fn increment_found_from(&self, data_name: &str) -> Result<()> {
        let q = format!("update found_from set {} = {} + 1", data_name, data_name);
        sqlx::query(&q).execute(&self.sqlitedb).await?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Thread {
    id: u64,
    guild_id: u64,
    channel_id: u64,
    title: String,
    history: String,
}
#[derive(Clone, Copy)]
struct SelectMenuSpec<'a> {
    value: &'a str,
    label: &'a str,
    display_emoji: &'a str,
    description: &'a str,
}

const NOT_INTRODUCED_QUESTION_COUNT: u8 = 5;
const QUESTION_COUNT: u8 = 3;

async fn safe_text(_ctx: &Context, _input: &String) -> String {
    content_safe(
        &_ctx.cache,
        _input,
        &ContentSafeOptions::default()
            .clean_channel(false)
            .clean_role(true)
            .clean_user(false),
        &[],
    )
}

async fn get_role(
    mci: &model::application::interaction::message_component::MessageComponentInteraction,
    ctx: &Context,
    name: &str,
) -> Role {
    let role = {
        if let Some(result) = mci
            .guild_id
            .unwrap()
            .to_guild_cached(&ctx.cache)
            .unwrap()
            .role_by_name(name)
        {
            result.clone()
        } else {
            let r = mci
                .guild_id
                .unwrap()
                .create_role(&ctx.http, |r| {
                    r.name(&name);
                    r.mentionable(false);
                    r.hoist(false);
                    r
                })
                .await
                .unwrap();
            r.clone()
        }
    };
    if role.name != "Member" && role.name != "Gitpodders" && !role.permissions.is_empty() {
        role.edit(&ctx.http, |r| r.permissions(Permissions::empty()))
            .await
            .unwrap();
    }
    role
}

async fn save_and_fetch_links(
    sites: &[&str],
    thread_id: u64,
    channel_id: u64,
    guild_id: u64,
    title: String,
    description: String,
) -> HashMap<String, String> {
    let mut links: HashMap<String, String> = HashMap::new();
    let client = reqwest::Client::new();
    let mclient = MeiliClient::new("http://localhost:7700", "optimus");
    let msettings = Settings::new()
        .with_searchable_attributes(["title", "description"])
        .with_distinct_attribute("title");
    mclient
        .index("threads")
        .set_settings(&msettings)
        .await
        .unwrap();
    let threads = mclient.index("threads");

    // Fetch matching links
    for site in sites.iter() {
        if let Ok(resp) = client
		.get(format!("https://www.google.com/search?q=site:{} {}", encode(site), encode(title.as_str())))
		.header("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/88.0.4324.182 Safari/537.36")
		.send()
		.await {
			if let Ok(result) = resp.text().await {
				let mut times = 1;
				// [^:~] avoids the google hyperlinks
				for caps in
					Regex::new(format!("\"(?P<url>{}/.[^:~]*?)\"", &site).as_str())
						.unwrap()
						.captures_iter(&result)
				{
					let url = &caps["url"];
					let hash = {
						if let Some(result) = Regex::new(r"(?P<hash>#[^:~].*)").unwrap().captures(url) {
							result.name("hash").map(|hash| hash.as_str())
						} else {
							None
						}
					};
					if let Ok(resp) = client.get(url).header("user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/88.0.4324.182 Safari/537.36")
					.send()
					.await {
						if let Ok(result) = resp.text().await {
							let result = html_escape::decode_html_entities(&result).to_string();
							for caps in Regex::new(r"<title>(?P<title>.*?)</title>").unwrap().captures_iter(&result) {
								let title = &caps["title"];
								let text = if hash.is_none() {
									title.to_string()
								} else {
									format!("{} | {}", title, hash.unwrap())
								};
								//links.push_str(format!("• __{}__\n\n", text).as_str());
								links.insert(text, url.to_string());
							}
						}
					}
					times += 1;
					if times > 3 {
						break;
					}
				}
			}
		}
    }

    // Fetch matching discord questions
    if let Ok(discord_questions) = threads
        .search()
        .with_query(format!("{} {}", title, description).as_str())
        .with_limit(3)
        .execute::<Thread>()
        .await
    {
        for ids in discord_questions.hits {
            links.insert(
                ids.result.title,
                format!(
                    "https://discord.com/channels/{}/{}/{}",
                    ids.result.guild_id, ids.result.channel_id, ids.result.id
                ),
            );
        }
    }

    // Save the question to search engine
    threads
        .add_documents(
            &[Thread {
                id: thread_id,
                channel_id,
                guild_id,
                title,
                history: description,
            }],
            Some("id"),
        )
        .await
        .ok();
    links
}

async fn close_issue(mci: &MessageComponentInteraction, ctx: &Context) {
    let _thread = mci.channel_id.edit_thread(&ctx.http, |t| t).await.unwrap();
    let thread_type = {
        if _thread.name.contains('✅') || _thread.name.contains('❓') {
            "question"
        } else {
            "thread"
        }
    };

    let thread_name = {
        if _thread.name.contains('✅') || thread_type == "thread" {
            _thread.name
        } else {
            format!("✅ {}", _thread.name.trim_start_matches("❓ "))
        }
    };
    let action_user_mention = mci.member.as_ref().unwrap().mention();
    let response = format!("This {} was closed by {}", thread_type, action_user_mention);
    mci.channel_id.say(&ctx.http, &response).await.unwrap();
    mci.create_interaction_response(&ctx.http, |r| {
        r.kind(InteractionResponseType::UpdateMessage);
        r.interaction_response_data(|d| d)
    })
    .await
    .unwrap();

    mci.channel_id
        .edit_thread(&ctx.http, |t| t.archived(true).name(thread_name))
        .await
        .unwrap();
}

async fn assign_roles(
    mci: &MessageComponentInteraction,
    ctx: &Context,
    role_choices: &Vec<String>,
    member: &mut Member,
    member_role: &Role
) {
    if role_choices.len() > 1 || !role_choices.iter().any(|x| x == "none") {
        // Is bigger than a single choice or doesnt contain none

        let test = member.roles.clone();

        let mut role_ids: Vec<RoleId> = Vec::new();
        for role_name in role_choices {
            if role_name == "none" {
                continue;
            }
            let role = get_role(mci, ctx, role_name.as_str()).await;
            if !test.contains(&role.id) {
                role_ids.push(role.id);
            }
        }
        member.add_roles(&ctx.http, &role_ids).await.unwrap();
        let db = &ctx.get_db().await;
        db.set_user_roles(mci.user.id, role_ids).await.unwrap();
    }

    // Add member role if missing
    if !member.roles.iter().any(|x| x == &member_role.id) {
        member.add_role(&ctx.http, member_role.id).await.unwrap();
    }
}

async fn show_issue_form(mci: &MessageComponentInteraction, ctx: &Context) {
    let db = &ctx.get_db().await;
    let desc = {
        if let Ok(result) = db
            .get_pending_question_content(&mci.user.id, &mci.channel_id)
            .await
        {
            db.remove_pending_question(&mci.user.id, &mci.channel_id)
                .await
                .ok();
            result
        } else {
            "".to_string()
        }
    };

    let channel_name = mci.channel_id.name(&ctx.cache).await.unwrap();
    mci.create_interaction_response(&ctx, |r| {
        r.kind(InteractionResponseType::Modal);
        r.interaction_response_data(|d| {
            d.custom_id("gitpod_help_button_press");
            d.title("Template");
            d.components(|c| {
                c.create_action_row(|ar| {
                    ar.create_input_text(|it| {
                        it.style(InputTextStyle::Short)
                            .custom_id("input_title")
                            .required(true)
                            .label("Title")
                            .max_length(98)
                    })
                });
                c.create_action_row(|ar| {
                    ar.create_input_text(|it| {
                        it.style(InputTextStyle::Paragraph)
                            .custom_id("input_description")
                            .label("Description")
                            .required(true)
                            .max_length(4000)
                            .value(desc)
                    })
                })
            })
        })
    })
    .await
    .unwrap();
}

pub async fn responder(ctx: Context, interaction: Interaction) {
    let ctx = &ctx.clone();

    match interaction {
        Interaction::MessageComponent(mci) => {
            match mci.data.custom_id.as_str() {
                "gitpod_create_issue" => show_issue_form(&mci, ctx).await,
                "gitpod_close_issue" => close_issue(&mci, ctx).await,
                "getting_started_letsgo" => {
                    let mut additional_roles: Vec<SelectMenuSpec> = Vec::from([
                        SelectMenuSpec {
                            value: "Newcomer",
                            description: "Get to know the people in the community",
                            label: "Newcomer",
                            display_emoji: "🌱",
                        },
                        SelectMenuSpec {
                            value: "Buidler",
                            description: "Find resources and share your work",
                            label: "Buidler",
                            display_emoji: "🏗️",
                        },
                        SelectMenuSpec {
                            value: "EarlyAdopter",
                            description: "Join the pioneers in the ecosystem",
                            label: "Early Adopter",
                            display_emoji: "🌅",
                        },
                        SelectMenuSpec {
                            value: "Governance",
                            description: "Take part in decision making processes",
                            label: "Governance",
                            display_emoji: "🏛️",
                        },
                        SelectMenuSpec {
                            value: "Research",
                            description: "Deep discussions between researchers",
                            label: "Academia and Research",
                            display_emoji: "🧑‍🔬",
                        },
                        SelectMenuSpec {
                            value: "Speculation",
                            description: "Markets, altcoins and degens",
                            label: "Speculation/Degen Stuff",
                            display_emoji: "🦍",
                        },
                        SelectMenuSpec {
                            value: "AllCategories",
                            description: "Just like the old times",
                            label: "Unlock everything",
                            display_emoji: "♾️",
                        },
                    ]);

                    let poll_entries: Vec<SelectMenuSpec> = Vec::from([
                        SelectMenuSpec {
                            value: "friend",
                            label: "Friend or colleague",
                            description: "A friend or colleague of mine introduced IOTA & Shimmer to me",
                            display_emoji: "🫂",
                        },
                        SelectMenuSpec {
                            value: "search_engine",
                            label: "Search Engine",
                            description: "I found IOTA & Shimmer through a search engine",
                            display_emoji: "🔎",
                        },
                        SelectMenuSpec {
                            value: "youtube",
                            label: "YouTube",
                            description: "Saw IOTA & Shimmer in a Youtube Video",
                            display_emoji: "📺",
                        },
                        SelectMenuSpec {
                            value: "twitter",
                            label: "Twitter",
                            description: "Saw people talking about IOTA & Shimmer on a Tweet",
                            display_emoji: "🐦",
                        },
                        SelectMenuSpec {
                            value: "market_cap",
                            label: "MarketCap",
                            description: "Found on CoinMarketCap/CoinGecko",
                            display_emoji: "✨",
                        },
                        SelectMenuSpec {
                            value: "meetup",
                            label: "Event",
                            description: "Participated in an IOTA & Shimmer event (Meetup, etc...)",
                            display_emoji: "🔗", 
                        }                       
                    ]);

                    let mut role_choices: Vec<String> = Vec::new();

                    // Get user and check if user already went threw onboarding once
                    let mut member = mci.member.clone().unwrap();
                    let member_role = get_role(&mci, ctx, "Onboarded").await;
                    let never_introduced = !member.roles.iter().any(|x| x == &member_role.id);

                    let mut question_index = 1;
                    let question_max = match never_introduced {
                        true => NOT_INTRODUCED_QUESTION_COUNT,
                        false => QUESTION_COUNT
                    };

                    mci.create_interaction_response(&ctx.http, |r| {
                    r.kind(InteractionResponseType::ChannelMessageWithSource);
                    r.interaction_response_data(|d| {
                        d.content(
                            format!("**[{}/{}]:** Which content would you like to have access to?", question_index, question_max),
                        );
                        d.components(|c| {
                            c.create_action_row(|a| {
                                a.create_select_menu(|s| {
                                    s.placeholder("Select your interest(s)");
                                    s.options(|o| {
										for spec in &additional_roles {
											o.create_option(|opt| {
												opt.label(spec.label);
												opt.description(spec.description);
												opt.emoji(ReactionType::Unicode(spec.display_emoji.to_string()));
												opt.value(spec.value)
											});
										}
                                        o.create_option(|opt| {
                                            opt.label("[Skip] I don't want any!")
                                                .description("Nope, I ain't need more.")
                                                .emoji(ReactionType::Unicode("⏭".to_string()))
                                                .value("none");
                                            opt
                                        });
                                        o
                                    });
                                    s.custom_id("channel_choice").max_values(additional_roles.len().try_into().unwrap())
                                });
                                a
                            });
                            c
                        });
                        d.custom_id("bruh")
                            .flags(MessageFlags::EPHEMERAL)
                    });
                    r
                })
                .await
                .unwrap();

                question_index = question_index + 1;

                    let mut interactions = mci
                        .get_interaction_response(&ctx.http)
                        .await
                        .unwrap()
                        .await_component_interactions(&ctx)
                        .timeout(Duration::from_secs(60 * 5))
                        .build();

                    while let Some(interaction) = interactions.next().await {
                        match interaction.data.custom_id.as_str() {
                            "channel_choice" => {
                                interaction.create_interaction_response(&ctx.http, |r| {
									r.kind(InteractionResponseType::UpdateMessage).interaction_response_data(|d|{
										d.content(
                                            format!("**[{}/{}]:** Would you like to get notified for community events?", question_index, question_max)
                                        );
										d.components(|c| {
											c.create_action_row(|a| {
												a.create_button(|b|{
													b.label("Yes!").custom_id("events").style(ButtonStyle::Success)
												});
												a.create_button(|b|{
													b.label("No, thank you!").custom_id("no_events").style(ButtonStyle::Danger)
												});
												a
											})
										});
										d
									})
								}).await.unwrap();

                                question_index = question_index + 1;

                                // Save the choices of last interaction
                                interaction
                                    .data
                                    .values
                                    .iter()
                                    .for_each(|x| role_choices.push(x.to_string()));
                            }
                            "events" | "no_events" => {
                                interaction.create_interaction_response(&ctx.http, |r| {
									r.kind(InteractionResponseType::UpdateMessage).interaction_response_data(|d|{
										d.content(
                                            format!("**[{}/{}]:** Would you like to get notified for polls and surveys?", question_index, question_max)
                                        );
										d.components(|c| {
											c.create_action_row(|a| {
												a.create_button(|b|{
													b.label("Yes!").custom_id("polls").style(ButtonStyle::Success)
												});
												a.create_button(|b|{
													b.label("No, thank you!").custom_id("no_polls").style(ButtonStyle::Danger)
												});
												a
											})
										});
										d
									})
								}).await.unwrap();

                                question_index = question_index + 1;

                                // Save the choices of last interaction
                                let event_role = SelectMenuSpec {
                                    label: "Events",
                                    description: "Subscribed to event pings",
                                    display_emoji: "",
                                    value: "Events",
                                };
                                if interaction.data.custom_id == "events" {
                                    role_choices.push(event_role.value.to_string());
                                }
                                additional_roles.push(event_role);
                            }
                            "polls" | "no_polls" => {
                                if !never_introduced {
                                    interaction.create_interaction_response(&ctx.http, |r| {
									    r.kind(InteractionResponseType::UpdateMessage).interaction_response_data(|d| {
										    d.content(
                                                format!("**[{}/{}]**: You have personalized the server, congrats!", question_index, question_max)
                                            ).components(|c|c)
									    })
								    }).await.unwrap();

                                    let final_msg = "Awesome, your server profile will be updated now!".to_owned();

                                    interaction
                                    .create_followup_message(&ctx.http, |d| {
                                        d.content(final_msg).components(|c| c);
                                        d.flags(MessageFlags::EPHEMERAL)
                                    })
                                    .await
                                    .unwrap();

                                    // Save the choices of last interaction
                                    let polls_role = SelectMenuSpec {
                                        label: "Polls",
                                        description: "Subscribed to event pings",
                                        display_emoji: "",
                                        value: "Polls",
                                    };
                                    if interaction.data.custom_id == "polls" {
                                        role_choices.push(polls_role.value.to_string());
                                    }
                                    additional_roles.push(polls_role);

                                    // Remove all roles which can be updated by  second Introduction run
                                    if let Some(roles) = member.roles(&ctx.cache) {
                                        // Remove all assignable roles first
                                        let mut all_assignable_roles: Vec<SelectMenuSpec> = Vec::new();
                                        all_assignable_roles.append(&mut additional_roles.clone());
                                        let mut removeable_roles: Vec<RoleId> = Vec::new();

                                        let subscribed_role = SelectMenuSpec {
                                            label: "Events",
                                            description: "Subscribed to event pings",
                                            display_emoji: "",
                                            value: "Events",
                                        };

                                        all_assignable_roles.push(subscribed_role);

                                        for role in roles {
                                            if all_assignable_roles.iter().any(|x| x.value == role.name)
                                            {
                                                removeable_roles.push(role.id);
                                            }
                                        }
                                        if !removeable_roles.is_empty() {
                                            member
                                                .remove_roles(&ctx.http, &removeable_roles)
                                                .await
                                                .unwrap();
                                        }
                                    }

                                    assign_roles(
                                        &mci,
                                        ctx,
                                        &role_choices,
                                        &mut member,
                                        &member_role,
                                    )
                                    .await;

                                    break;
                                }

                                interaction.create_interaction_response(&ctx.http, |r| {
									r.kind(InteractionResponseType::UpdateMessage).interaction_response_data(|d| {
										d.content(
                                            format!("**[{}/{}]:** Why did you join our community?", question_index, question_max)
                                        ).components(|c| {
											c.create_action_row(|a| {
												a.create_button(|b|{
													b.label("To hangout with others");
													b.style(ButtonStyle::Secondary);
													b.emoji(ReactionType::Unicode("🏄".to_string()));
													b.custom_id("hangout")
												});
												a.create_button(|b|{
													b.label("To get help with IOTA & Shimmer");
													b.style(ButtonStyle::Secondary);
													b.emoji(ReactionType::Unicode("✌️".to_string()));
													b.custom_id("help")
												});
												a.create_button(|b|{
													b.label("To develop on IOTA & Shimmer");
													b.style(ButtonStyle::Secondary);
													b.emoji(ReactionType::Unicode("🏡".to_string()));
													b.custom_id("develop")
												});
												a
											})
										})
									})
								}).await.unwrap();

                                question_index = question_index + 1;

                                // Save the choices of last interaction
                                let polls_role = SelectMenuSpec {
                                    label: "Polls",
                                    description: "Subscribed to event pings",
                                    display_emoji: "",
                                    value: "Polls",
                                };
                                if interaction.data.custom_id == "polls" {
                                    role_choices.push(polls_role.value.to_string());
                                }
                                additional_roles.push(polls_role);
                            }
                            "hangout" | "help" | "develop" => {
                                interaction.create_interaction_response(&ctx.http, |r| {
									r.kind(InteractionResponseType::UpdateMessage).interaction_response_data(|d| {
										d.content(
                                            format!("**[{}/{}]**: You have personalized the server, congrats!", question_index, question_max)
                                        ).components(|c|c)
									})
								}).await.unwrap();

                                let followup = interaction
                                    .create_followup_message(&ctx.http, |d| {
                                        d.content(
                                            format!("**[{}/{}]:** How did you find IOTA & Shimmer?", question_index, question_max)
                                        );
                                        d.components(|c| {
                                            c.create_action_row(|a| {
                                                a.create_select_menu(|s| {
                                                    s.placeholder(
                                                        "[Poll]: Select sources (Optional)",
                                                    );
                                                    s.options(|o| {
                                                        for spec in &poll_entries {
                                                            o.create_option(|opt| {
                                                                opt.label(spec.label);
                                                                opt.description(spec.description);
                                                                opt.emoji(ReactionType::Unicode(
                                                                    spec.display_emoji.to_string(),
                                                                ));
                                                                opt.value(spec.value);
                                                                opt
                                                            });
                                                        }
                                                        o.create_option(|opt| {
                                                            opt.label("[Skip] Prefer not to share")
                                                                .value("none")
                                                                .emoji(ReactionType::Unicode(
                                                                    "⏭".to_string(),
                                                                ));
                                                            opt
                                                        });
                                                        o
                                                    });
                                                    s.custom_id("found_gitpod_from").max_values(5)
                                                });
                                                a
                                            });
                                            c
                                        });
                                        d.flags(MessageFlags::EPHEMERAL)
                                    })
                                    .await
                                    .unwrap();

                                let followup_results = match followup
                                    .await_component_interaction(&ctx)
                                    .timeout(Duration::from_secs(60 * 5))
                                    .await
                                {
                                    Some(ci) => {
                                        let final_msg = {
                                            if never_introduced {
                                                MessageBuilder::new()
												.push_line(format!(
													"Thank you {}! If you'd like to get more introduction info, drop by {} and say Hi :)",
													interaction.user.mention(),
													INTRODUCTION_CHANNEL.mention()
												))
												.push_line("\nWe’d love to get to know you better and hear about:")
                                                .push_quote_line("🌈 your favourite IOTA & Shimmer feature")
												.push_quote_line("🔧 what you’re working on!").build()
                                            } else {
                                                "Awesome, your server profile will be updated now!"
                                                    .to_owned()
                                            }
                                        };
                                        ci.create_interaction_response(&ctx.http, |r| {
                                            r.kind(InteractionResponseType::UpdateMessage)
                                                .interaction_response_data(|d| {
                                                    d.content(final_msg).components(|c| c)
                                                })
                                        })
                                        .await
                                        .unwrap();
                                        ci
                                    }
                                    None => return,
                                };

                                // save the found from data
                                let db = ctx.get_db().await;

                                for result in followup_results.data.values.iter() {
                                    println!("{}", &result);
                                    db.increment_found_from(&result).await;
                                }

                                // Remove old roles
                                if let Some(roles) = member.roles(&ctx.cache) {
                                    // Remove all assignable roles first
                                    let mut all_assignable_roles: Vec<SelectMenuSpec> = Vec::new();
                                    all_assignable_roles.append(&mut additional_roles.clone());
                                    let mut removeable_roles: Vec<RoleId> = Vec::new();

                                    for role in roles {
                                        if all_assignable_roles.iter().any(|x| x.value == role.name)
                                        {
                                            removeable_roles.push(role.id);
                                        }
                                    }
                                    if !removeable_roles.is_empty() {
                                        member
                                            .remove_roles(&ctx.http, &removeable_roles)
                                            .await
                                            .unwrap();
                                    }
                                }

                                let db = &ctx.get_db().await;
                                db.increment_join_reason(interaction.data.custom_id.as_str())
                                    .await;

                                assign_roles(&mci, ctx, &role_choices, &mut member, &member_role)
                                .await;

                                // save the found from data
                                followup_results
                                    .data
                                    .values
                                    .iter()
                                    .for_each(|x| role_choices.push(x.to_string()));

                                // Remove old roles
                                if let Some(roles) = member.roles(&ctx.cache) {
                                    // Remove all assignable roles first
                                    let mut all_assignable_roles: Vec<SelectMenuSpec> = Vec::new();
                                    all_assignable_roles.append(&mut additional_roles.clone());
                                    let mut removeable_roles: Vec<RoleId> = Vec::new();

                                    for role in roles {
                                        if all_assignable_roles.iter().any(|x| x.value == role.name)
                                        {
                                            removeable_roles.push(role.id);
                                        }
                                    }
                                    if !removeable_roles.is_empty() {
                                        member
                                            .remove_roles(&ctx.http, &removeable_roles)
                                            .await
                                            .unwrap();
                                    }
                                }

                                assign_roles(&mci, ctx, &role_choices, &mut member, &member_role)
                                .await;

                                if never_introduced {
                                    // Wait for the submittion on INTRODUCTION_CHANNEL
                                    if let Some(msg) = mci
                                        .user
                                        .await_reply(&ctx)
                                        .timeout(Duration::from_secs(60 * 30))
                                        .await
                                    {
                                        // Watch intro channel
                                        if msg.channel_id == INTRODUCTION_CHANNEL {
                                            // let mut count = 0;
                                            // intro_msgs.iter().for_each(|x| {
                                            // 	if x.author == msg.author {
                                            // 		count += 1;
                                            // 	}
                                            // });

                                            // if count <= 1 {
                                            let thread = msg
                                                .channel_id
                                                .create_public_thread(&ctx.http, &msg.id, |t| {
                                                    t.auto_archive_duration(1440).name(format!(
                                                        "Welcome {}!",
                                                        msg.author.name
                                                    ))
                                                })
                                                .await
                                                .unwrap();

                                            if words_count::count(&msg.content).words > 5 {
                                                msg.react(
                                                    &ctx.http,
                                                    ReactionType::Unicode("🔥".to_string()),
                                                )
                                                .await
                                                .unwrap();
                                            }
                                            msg.react(
                                                &ctx.http,
                                                ReactionType::Unicode("👋".to_string()),
                                            )
                                            .await
                                            .unwrap();

                                            let general_channel = if cfg!(debug_assertions) {
                                                ChannelId(947769443516284943)
                                            } else {
                                                ChannelId(970953101894889523)
                                            };
                                            let offtopic_channel = if cfg!(debug_assertions) {
                                                ChannelId(947769443793141769)
                                            } else {
                                                ChannelId(970953101894889529)
                                            };
                                            let db = &ctx.get_db().await;
                                            let questions_channel =
                                                db.get_question_channels().await.unwrap();
                                            let questions_channel =
                                                questions_channel.into_iter().next().unwrap().id;

                                            let selfhosted_questions_channel =
                                                if cfg!(debug_assertions) {
                                                    ChannelId(947769443793141761)
                                                } else {
                                                    ChannelId(879915120510267412)
                                                };

                                            let mut prepared_msg = MessageBuilder::new();
                                            prepared_msg.push_line(format!(
                                                "Welcome to the IOTA & Shimmer community {} 🙌\n",
                                                &msg.author.mention()
                                            ));

                                            prepared_msg.push_bold_line("Here are some channels that you should check out:")
											.push_quote_line(format!("• {} - for anything IOTA & Shimmer related", &general_channel.mention()))
											.push_quote_line(format!("• {} - for any random discussions ☕️", &offtopic_channel.mention()))
											.push_quote_line(format!("• {} - have a question or need help? This is the place to ask! ❓\n", &questions_channel.mention()))
											.push_line("…And there’s more! Take your time to explore :)\n")
											.push_bold_line("Feel free to check out the following pages to learn more about IOTA & Shimmer:")
											.push_quote_line("• <https://www.iota.org>")
                                            .push_quote_line("• <https://shimmer.network>")
											.push_quote_line("• <https://wiki.iota.org>");

                                            let mut extra_msg = MessageBuilder::new();
                                            if role_choices.contains(&additional_roles[6].value.to_string()) {
                                                extra_msg.push(welcome_all());
                                            }
                                            else {
                                                if role_choices.contains(&additional_roles[0].value.to_string()) {
                                                    extra_msg.push(welcome_newcomer());
                                                }
                                                if role_choices.contains(&additional_roles[1].value.to_string()) {
                                                    extra_msg.push(welcome_buidler());
                                                }
                                                if role_choices.contains(&additional_roles[2].value.to_string()) {
                                                    extra_msg.push(welcome_eary_adopter());
                                                }
                                                if role_choices.contains(&additional_roles[3].value.to_string()) {
                                                    extra_msg.push(welcome_governance());
                                                }
                                                if role_choices.contains(&additional_roles[4].value.to_string()) {
                                                    extra_msg.push(welcome_researcher());
                                                }
                                                if role_choices.contains(&additional_roles[5].value.to_string()) {
                                                    extra_msg.push(welcome_speculator());
                                                }
                                            }

                                            let mut thread_msg = thread
                                                .send_message(&ctx.http, |t| {
                                                    t.content(prepared_msg)
                                                })
                                                .await
                                                .unwrap();
                                            thread_msg.suppress_embeds(&ctx.http).await.unwrap();
                                            thread_msg = thread
                                                .send_message(&ctx.http, |t| {
                                                    t.content(extra_msg)
                                                })
                                                .await
                                                .unwrap();
                                            thread_msg.suppress_embeds(&ctx.http).await.unwrap();
                                            // } else {
                                            // 	let warn_msg = msg
                                            // 	.reply_mention(
                                            // 		&ctx.http,
                                            // 		"Please reply in threads above instead of here",
                                            // 	)
                                            // 	.await
                                            // 	.unwrap();
                                            // 	sleep(Duration::from_secs(10)).await;
                                            // 	warn_msg.delete(&ctx.http).await.unwrap();
                                            // 	msg.delete(&ctx.http).await.ok();
                                            // }
                                        }
                                        // }
                                    }
                                }

                                break;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {
                    // If a Question thread suggestion was clicked
                    if mci.data.custom_id.starts_with("http") {
                        let button_label = &mci
                            .message
                            .components
                            .iter()
                            .find_map(|a| {
                                a.components.iter().find_map(|x| {
                                    let button: Button =
                                        serde_json::from_value(serde_json::to_value(x).unwrap())
                                            .unwrap();
                                    if button.custom_id.unwrap() == mci.data.custom_id {
                                        Some(button.label.unwrap())
                                    } else {
                                        None
                                    }
                                })
                            })
                            .unwrap();

                        mci.create_interaction_response(&ctx.http, |r| {
                            r.kind(InteractionResponseType::ChannelMessageWithSource)
                                .interaction_response_data(|d| {
                                    d.content(format!("{}: {button_label}", &mci.user.mention()))
                                        .components(|c| {
                                            c.create_action_row(|a| {
                                                a.create_button(|b| {
                                                    b.label("Open link")
                                                        .url(&mci.data.custom_id)
                                                        .style(ButtonStyle::Link)
                                                })
                                            })
                                        })
                                        .flags(MessageFlags::EPHEMERAL)
                                })
                        })
                        .await
                        .unwrap();

                        mci.message
                            .react(&ctx.http, ReactionType::Unicode("🔎".to_string()))
                            .await
                            .unwrap();
                    }
                }
            }
        }
        Interaction::ApplicationCommand(mci) => match mci.data.name.as_str() {
            "close" => {
                let _thread = mci.channel_id.edit_thread(&ctx.http, |t| t).await.unwrap();
                let thread_type = {
                    if _thread.name.contains('✅') || _thread.name.contains('❓') {
                        "question"
                    } else {
                        "thread"
                    }
                };
                mci.create_interaction_response(&ctx.http, |r| {
                    r.kind(InteractionResponseType::ChannelMessageWithSource);
                    r.interaction_response_data(|d| {
                        d.content(format!("This {} was closed", thread_type))
                    })
                })
                .await
                .unwrap();
                let thread_node = mci.channel_id.edit_thread(&ctx.http, |t| t).await.unwrap();
                let thread_name = {
                    if thread_node.name.contains('✅') || thread_type == "thread" {
                        thread_node.name
                    } else {
                        format!("✅ {}", thread_node.name.trim_start_matches("❓ "))
                    }
                };
                mci.channel_id
                    .edit_thread(&ctx.http, |t| t.archived(true).name(thread_name))
                    .await
                    .unwrap();
            }
            "nothing_to_see_here" => {
                let input = mci
                    .data
                    .options
                    .get(0)
                    .expect("Expected input")
                    .value
                    .as_ref()
                    .unwrap();
                mci.create_interaction_response(&ctx.http, |r| {
                    r.kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|d| {
                            d.content("Posted message on this channel")
                                .flags(MessageFlags::EPHEMERAL)
                        })
                })
                .await
                .unwrap();

                mci.channel_id
                    .send_message(&ctx.http, |m| {
                        m.content(
                            input
                                .to_string()
                                .trim_start_matches('"')
                                .trim_end_matches('"'),
                        )
                    })
                    .await
                    .unwrap();
            }
            _ => {}
        },
        Interaction::ModalSubmit(mci) => {
            let typing = mci.channel_id.start_typing(&ctx.http).unwrap();
            let title = match mci
                .data
                .components
                .get(0)
                .unwrap()
                .components
                .get(0)
                .unwrap()
            {
                ActionRowComponent::InputText(it) => it,
                _ => return,
            };
            let description = match mci
                .data
                .components
                .get(1)
                .unwrap()
                .components
                .get(0)
                .unwrap()
            {
                ActionRowComponent::InputText(it) => it,
                _ => return,
            };

            mci.create_interaction_response(ctx, |r| {
                if mci.data.custom_id == "gitpod_help_button_press" {
                    r.kind(InteractionResponseType::ChannelMessageWithSource);
                    r.interaction_response_data(|d| d)
                } else {
                    r.kind(InteractionResponseType::UpdateMessage);
                    r.interaction_response_data(|d| d)
                }
            })
            .await
            .ok();

            let user_name = &mci.user.name;
            let channel_name = &mci.channel_id.name(&ctx.cache).await.unwrap();
            // let self_avatar = &ctx.cache.current_user().await.face();
            // let self_name = &ctx.cache.current_user().await.name;
            let webhook_get = mci.channel_id.webhooks(&ctx).await.unwrap();
            for hook in webhook_get {
                if hook.name == Some(user_name.clone()) {
                    hook.delete(&ctx).await.unwrap();
                }
            }

            let img_url = reqwest::Url::parse(&mci.user.face().replace(".webp", ".png")).unwrap();
            let webhook = mci
                .channel_id
                .create_webhook_with_avatar(&ctx, &user_name, AttachmentType::Image(img_url))
                .await
                .unwrap();

            let temp_embed = Embed::fake(|e| e.description(&description.value));

            let mut msg = webhook
                .execute(&ctx, true, |w| {
                    w.embeds(vec![temp_embed]).content(&title.value)
                })
                .await
                .unwrap()
                .unwrap();
            msg.suppress_embeds(&ctx.http).await.unwrap();
            webhook.delete(&ctx.http).await.unwrap();
            typing.stop().unwrap();
            if mci.data.custom_id == "gitpod_help_button_press" {
                if let Some(msg) = mci.message {
                    msg.delete(&ctx.http).await.ok();
                }
            }

            let user_mention = mci.user.mention();

            let thread_auto_archive_dur = {
                if cfg!(debug_assertions) {
                    1440 // 1 day
                } else {
                    4320 // 3 days
                }
            };

            let thread = mci
                .channel_id
                .create_public_thread(&ctx, msg.id, |e| {
                    e.name(format!("❓ {}", &title.value))
                        .auto_archive_duration(thread_auto_archive_dur)
                })
                .await
                .unwrap();

            let desc_safe = safe_text(ctx, &description.value).await;
            thread
                .send_message(&ctx.http, |m| {
                    if description.value.chars().count() < 1960 {
                        m.content(
                            MessageBuilder::new()
                                .push_underline_line("**Description**")
                                .push_line(&desc_safe)
                                .push_bold("---------------")
                                .build(),
                        );
                    } else {
                        m.add_embed(|e| e.title("Description").description(desc_safe));
                    }

                    m
                })
                .await
                .unwrap();

            thread
                .send_message(&ctx, |m| {
                    m.content( MessageBuilder::new().push_quote(format!("Hey {}! Thank you for raising this — please hang tight as someone from our community may help you out. Meanwhile, feel free to add anymore information in this thread!", user_mention)).build()).components(|c| {
                        c.create_action_row(|ar| {
                            ar.create_button(|button| {
                                button
                                    .style(ButtonStyle::Danger)
                                    .label("Close")
                                    .custom_id("gitpod_close_issue")
                                    .emoji(ReactionType::Unicode("🔒".to_string()))
                            })
                        })
                    })
                })
                .await
                .unwrap();

            questions_thread::responder(ctx).await;

            let thread_typing = thread.clone().start_typing(&ctx.http).unwrap();
            let mut relevant_links = save_and_fetch_links(
                &["https://www.gitpod.io/docs", "https://github.com/gitpod-io"],
                *thread.id.as_u64(),
                *mci.channel_id.as_u64(),
                *mci.guild_id.unwrap().as_u64(),
                (*title.value).to_string(),
                (*description.value).to_string(),
            )
            .await;
            if !&relevant_links.is_empty() {
                let mut prefix_emojis: HashMap<&str, Emoji> = HashMap::new();
                let emoji_sources: HashMap<&str, &str> = HashMap::from([
					("gitpod", "https://www.gitpod.io/images/media-kit/logo-mark.png"),
					("github", "https://cdn.discordapp.com/attachments/981191970024210462/981192908780736573/github-transparent.png"),
					("discord", "https://discord.com/assets/9f6f9cd156ce35e2d94c0e62e3eff462.png")
				]);
                let guild = &mci.guild_id.unwrap();
                for source in ["gitpod", "github", "discord"].iter() {
                    let emoji = {
                        if let Some(emoji) = guild
                            .emojis(&ctx.http)
                            .await
                            .unwrap()
                            .into_iter()
                            .find(|x| x.name == *source)
                        {
                            emoji
                        } else {
                            let dw_path = env::current_dir().unwrap().join(format!("{source}.png"));
                            let dw_url = emoji_sources.get(source).unwrap().to_string();
                            let client = reqwest::Client::new();
                            let downloaded_bytes = client
                                .get(dw_url)
                                .timeout(Duration::from_secs(5))
                                .send()
                                .await
                                .unwrap()
                                .bytes()
                                .await
                                .unwrap();
                            tokio::fs::write(&dw_path, &downloaded_bytes).await.unwrap();
                            let emoji_image = read_image(dw_path).unwrap();
                            let emoji_image = emoji_image.as_str();
                            guild
                                .create_emoji(&ctx.http, source, emoji_image)
                                .await
                                .unwrap()
                        }
                    };
                    prefix_emojis.insert(source, emoji);
                }

                let mut suggested_count = 1;
                thread.send_message(&ctx.http, |m| {
				m.content(format!("{} I also found some relevant links which might answer your question, please do check them out below 🙏:", &user_mention));
					m.components(|c| {
						loop {
							if suggested_count > 10 || relevant_links.is_empty() {
								break;
							}
							c.create_action_row(|a|
								{
									let mut i = 1;
									for (title, url) in relevant_links.clone() {
										if i > 5 {
											break;
										} else {
											i += 1;
											relevant_links.remove(&title);
										}
										let emoji = {
											if url.starts_with("https://www.gitpod.io") {
												prefix_emojis.get("gitpod").unwrap()
											} else if url.starts_with("https://github.com") {
												prefix_emojis.get("github").unwrap()
											} else {
												prefix_emojis.get("discord").unwrap()
											}
										};

										a.create_button(|b|b.label(&title.as_str().substring(0, 80)).custom_id(&url.as_str().substring(0, 100)).style(ButtonStyle::Secondary).emoji(ReactionType::Custom {
											id: emoji.id,
											name: Some(emoji.name.clone()),
											animated: false,
										}));
									}
										a
									}
								);
								suggested_count += 1;
						}
							c
						});
						m
					}
				).await.unwrap();
                thread_typing.stop().unwrap();
            }
            // if !relevant_links.is_empty() {
            //     thread
            //         .send_message(&ctx.http, |m| 
            //             m.content(format!(
            //                 "{} I also found some relevant links which might answer your question, please do check them out below 🙏:",
            //                 &user_mention
            //             ))
            //             .embed(|e| e.description(relevant_links))
            //         })
            //         .await
            //         .unwrap();
            //     thread_typing.stop();
            // }
            // let db = &ctx.get_db().await;
            // db.add_title(i64::from(mci.id), &title.value).await.unwrap();
        }
        _ => (),
    }
}

fn welcome_all() -> MessageBuilder {
    let mut msg = MessageBuilder::new();
    msg.push_bold_line("Don't wanna miss a thing, eh?")
    .push_line("Get ready to unlock the whole potential of the server.

    👇")
    .push_line("")

    .push(welcome_newcomer())
    .push(welcome_buidler())
    .push(welcome_eary_adopter())
    .push(welcome_governance())
    .push(welcome_researcher())
    .push(welcome_speculator());

    msg
}

fn welcome_newcomer() -> MessageBuilder {
    let mut msg = MessageBuilder::new();
    msg.push_bold_line("Hello and welcome to your community")
    .push_line("- Browse the channels and feel free to ask questions to learn more.
    - Not all activitiy is visible right now. Get dedicated roles to unlock more channels in <#884705920028930068>.")
    .push_line("");

    msg
}

fn welcome_buidler() -> MessageBuilder {
    let mut msg = MessageBuilder::new();
    msg.push_bold_line("Ready to buidl?")
    .push_line("We suggest to start with the wiki at <https://wiki.iota.org>.
    - Currently you may be interested in Stardust, the first iteration of the Shimmer innovation network with support for a multi asset DLT. <https://wiki.iota.org/introduction/develop/welcome>.
    - For a quick start have a look at our tutorial section: <https://wiki.iota.org/tutorials>.")
    .push_line("");

    msg
}

fn welcome_eary_adopter() -> MessageBuilder {
    let mut msg = MessageBuilder::new();
    msg.push_bold_line("The early bird catches the worm")
    .push_line("- Hangout with the community, explore and try out upcoming dApps and opportunities.
    - Explore ecosystem projects: <https://shimmer.network/ecosystem>
    - Be informed about the newest development proposals early, have a look at our Tangle Improvement Proposals (TIPs) repo https://github.com/iotaledger/tips
     - Already heared about our Touchpoint initiative to build, launch and scale the next generation of dApps and infrastructure? Learn more: <https://assembly.sc/touchpoint>")
    .push_line("");
    
    msg
}

fn welcome_governance() -> MessageBuilder {
    let mut msg = MessageBuilder::new();
    msg.push_bold_line("Ready to take matter in your own hands?")
    .push_line("The community is empowered to take part in governance. Start participating in key decisions at our governance forum <https://govern.iota.org>")
    .push_line("");
    
    msg
}

fn welcome_researcher() -> MessageBuilder {
    let mut msg = MessageBuilder::new();
    msg.push_bold_line("We build on the shoulders of giants")
    .push_line("Research is a key element to the project.
    - Have a look at our research papers https://wiki.iota.org/research/research-papers
    - Keep yourself up-to-date with the latest coordicide specs https://wiki.iota.org/IOTA-2.0-Research-Specifications/Preface
    And join the discussion in <#970953102503071780>")
    .push_line("");
    
    msg
}

fn welcome_speculator() -> MessageBuilder {
    let mut msg = MessageBuilder::new();
    msg.push_bold_line("Ready to ape in?")
    .push_line("- Take off your shoes and join <#970953101894889530>. Where big 🧠 start as degens 🦍 and become regens  (And don't forget to give *p* bot some love)
    - Discuss other projects eloquently in <#970953101894889531>")
    .push_line("");
    
    msg
}
