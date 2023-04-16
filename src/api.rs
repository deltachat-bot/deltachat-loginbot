use crate::config::NotifierConfig;

pub struct TopicData {
    archetype: String,
    details: Details,
}

pub struct Details {
    allowed_users: Vec<User>,
}

pub struct User {
    id: String,
}

pub struct Api {
    config: NotifierConfig,
    active_user_endpoint: String,
    topic_endpoint: String,
    posts_endpoint: String,
}

impl Api {
    pub fn new(config: NotifierConfig) -> Self {
        Api {
            config,
            active_user_endpoint: "/admin/users/list/active.json?filter={}".to_string(),
            topic_endpoint: "/t/{}.json".to_string(),
            posts_endpoint: "posts.json".to_string(),
        }
    }
    
    pub async fn get_active_users_by_email(&self, email: String) -> Result<Vec<String>> {
        let client = reqwest::Client::new();
        client.get(format!("{}{}", self.active_user_endpoint, email)).await?.json::<Vec<String>>().await
    }
    
    pub async fn get_topic_by_id(&self, id: String) -> Result<TopicData> {
        let client = reqwest::Client::new();
        client.get(format!(self.topic_endpoint, id)).await?.json::<TopicData>().await
    }

    pub async fn create_post(&self, payload: Post, username: Option<String>) -> Result<()> {
        let username = username.unwrap_or(self.config.api_username);
        let client = reqwest::Client::new();
        client.post(self.posts_endpoint).json(payload).send().await?;
        Ok(())
    }
}
