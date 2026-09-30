#[derive(Debug, Clone)]
pub struct Detection<'a> {
    pub bot: Option<Bot<'a>>,
    pub os: Option<Os<'a>>,
    pub client: Option<Client<'a>>,
    pub device: Option<Device<'a>>,
}

impl<'a> Detection<'a> {
    pub fn is_bot(&self) -> bool {
        self.bot.is_some()
    }
    pub fn bot(&self) -> Option<&Bot<'a>> {
        self.bot.as_ref()
    }
    pub fn os(&self) -> Option<&Os<'a>> {
        self.os.as_ref()
    }
    pub fn client(&self) -> Option<&Client<'a>> {
        self.client.as_ref()
    }
    pub fn device(&self) -> Option<&Device<'a>> {
        self.device.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct Bot<'a> {
    pub name: ::std::borrow::Cow<'a, str>,
    pub category: Option<&'a str>,
    pub url: Option<&'a str>,
    pub producer: Option<BotProducer<'a>>,
}

#[derive(Debug, Clone)]
pub struct BotProducer<'a> {
    pub name: Option<&'a str>,
    pub url: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Os<'a> {
    pub name: ::std::borrow::Cow<'a, str>,
    pub version: ::std::borrow::Cow<'a, str>,
}

#[derive(Debug, Clone)]
pub struct Client<'a> {
    pub kind: super::ClientType,
    pub name: ::std::borrow::Cow<'a, str>,
    pub version: ::std::borrow::Cow<'a, str>,
    pub engine: ::std::borrow::Cow<'a, str>,
    pub engine_version: ::std::borrow::Cow<'a, str>,
}

#[derive(Debug, Clone)]
pub struct Device<'a> {
    pub kind: Option<super::DeviceType>,
    pub brand: ::std::borrow::Cow<'a, str>,
    pub model: ::std::borrow::Cow<'a, str>,
}