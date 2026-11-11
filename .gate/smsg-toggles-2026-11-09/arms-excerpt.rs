            "smsgenable" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                if params.get(0).is_some() {
                    return Err((-8, "smsgenable takes no parameters".into()));
                }
                if engine.enabled() {
                    return Err((-1, "Secure messaging is already enabled.".into()));
                }
                engine.enable(now_secs() as i64);
                log("smsg: enabled via RPC (SecureMsgEnable parity)");
                Ok(serde_json::json!({ "result": "Enabled secure messaging." }))
            }
            "smsgdisable" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
                };
                if params.get(0).is_some() {
                    return Err((-8, "smsgdisable takes no parameters".into()));
                }
                if !engine.enabled() {
                    return Err((-1, "Secure messaging is already disabled.".into()));
                }
                engine.disable();
                log("smsg: disabled via RPC (SecureMsgDisable parity)");
                Ok(serde_json::json!({ "result": "Disabled secure messaging." }))
            }
            "smsgoptions" => {
                let Some(engine) = &self.smsg else {
                    return Err((-1, SMSG_NOT_WIRED.into()));
