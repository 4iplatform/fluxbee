use async_trait::async_trait;

use crate::errors::Result;
use crate::message::Message;

#[async_trait]
pub trait AiNode: Send + Sync {
    async fn on_message(&self, msg: Message) -> Result<Option<Message>>;
}
