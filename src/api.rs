

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
            active_user_endpoint: "/admin/users/list/active.json?filter={}",
            topic_endpoint: "/t/{}.json",
            posts_endpoint: "posts.json",
        }
    }
    
    pub fn get_active_users_by_email(&self, email: String) {
        reqwest::get(format!(self.active_user_endpoint, email))
            //...
    }
}
