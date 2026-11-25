"""Private bounded worker admission cache from stopped CLI quota diagnostics.

It can only withhold admission. PostgreSQL and pinned recipes remain authority;
this cache never changes a task/model or obtains allowance from a provider.
"""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import stat
import time
from uuid import UUID,uuid4

from work_policy import AGY_POLICY_ID
from work_config import validate_identifier,validate_model
from work_quota_diagnostics import quota_summary

MAX_BYTES=262144
MAX_ENTRIES=320
LOCK_SECONDS=0.25


class ProviderCooldown:
    def __init__(self,config,*,clock=time.time):self.config,self.clock=config,clock
    def prepare(self):
        """Acquire private worker state before other ready ports create children."""
        os.close(self._state_directory())
    def _state_directory(self):
        """Acquire the selected state root without following untrusted parents.

        Only its final component may be created. Existing root/user-owned
        ancestors must not be writable by others, except a root-owned sticky
        directory such as /tmp. The selected root itself must be worker-owned.
        """
        flags=os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW
        directory=os.open('/',flags)
        try:
            parts=self.config.state_root.parts[1:]
            for ordinal,part in enumerate(parts):
                final=ordinal==len(parts)-1
                try:current=os.open(part,flags,dir_fd=directory)
                except FileNotFoundError:
                    if not final:raise ValueError('Provider state root parent is missing')from None
                    try:os.mkdir(part,mode=0o700,dir_fd=directory)
                    except FileExistsError:pass # A concurrent first reader may create it.
                    else:os.fsync(directory)
                    current=os.open(part,flags,dir_fd=directory)
                try:
                    info=os.fstat(current)
                    writable=bool(info.st_mode&0o022)
                    sticky_parent=not final and info.st_uid==0 and bool(info.st_mode&stat.S_ISVTX)
                    if (not stat.S_ISDIR(info.st_mode)or info.st_uid not in({os.geteuid()}if final else{0,os.geteuid()})
                        or writable and not sticky_parent):raise ValueError('Unsafe provider state root or ancestor')
                except BaseException:
                    os.close(current);raise
                os.close(directory);directory=current
            selected=directory;directory=None
            return selected
        finally:
            if directory is not None:os.close(directory)
    @contextmanager
    def _locked(self):
        state=self._state_directory()
        try:
            try:os.mkdir('provider-cooldowns',mode=0o700,dir_fd=state)
            except FileExistsError:pass
            else:os.fsync(state)
            directory=os.open('provider-cooldowns',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=state)
        finally:os.close(state)
        lock=None
        try:
            info=os.fstat(directory)
            if not stat.S_ISDIR(info.st_mode)or info.st_uid!=os.geteuid()or stat.S_IMODE(info.st_mode)!=0o700:
                raise ValueError('Unsafe provider cooldown directory')
            lock=os.open('admission.lock',os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW|os.O_NONBLOCK,0o600,dir_fd=directory)
            info=os.fstat(lock)
            if not stat.S_ISREG(info.st_mode)or info.st_uid!=os.geteuid()or stat.S_IMODE(info.st_mode)!=0o600 or info.st_nlink!=1:
                raise ValueError('Unsafe provider cooldown lock')
            deadline=time.monotonic()+LOCK_SECONDS
            while True:
                try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB);break
                except BlockingIOError:
                    remaining=deadline-time.monotonic()
                    if remaining<=0:raise
                    time.sleep(min(0.01,remaining))
            try:
                fd=os.open('state.json',os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=directory)
            except FileNotFoundError:value={'version':1,'entries':[]}
            else:
                with os.fdopen(fd,'rb')as stream:
                    info=os.fstat(stream.fileno())
                    if not stat.S_ISREG(info.st_mode)or info.st_uid!=os.geteuid()or stat.S_IMODE(info.st_mode)!=0o600 or info.st_nlink!=1 or info.st_size>MAX_BYTES:
                        raise ValueError('Unsafe provider cooldown state')
                    from work_external_review_context import strict_json
                    value=strict_json(stream.read(MAX_BYTES+1))
            if(not isinstance(value,dict)or set(value)!={'version','entries'}or type(value['version'])is not int or value['version']!=1
                or not isinstance(value['entries'],list)or len(value['entries'])>MAX_ENTRIES):raise ValueError('Invalid provider cooldown state')
            scopes=set();attempts=set()
            for item in value['entries']:
                if(not isinstance(item,dict)or set(item)!={'profile','model','attempt_id','blocked_until','diagnostic'}
                    or type(item['blocked_until'])not in (int,float)or not 0<item['blocked_until']<2**53
                    or not isinstance(item['attempt_id'],str)
                    or str(UUID(item['attempt_id']))!=item['attempt_id']):raise ValueError('Invalid provider cooldown entry')
                validate_identifier(item['profile'],'cooldown profile');validate_model(item['model'])
                quota_summary(item['diagnostic'])
                scope=(item['profile'],item['model'])
                if scope in scopes or item['attempt_id']in attempts:raise ValueError('Duplicate provider cooldown entry')
                scopes.add(scope);attempts.add(item['attempt_id'])
            yield directory,value
        finally:
            if lock is not None:os.close(lock)
            os.close(directory)
    def record(self,lease,diagnostic):
        quota_summary(diagnostic)
        profile=lease['agent_profile'];model=lease['recipe']['model'];attempt=str(UUID(lease['attempt_id']))
        if(profile not in self.config.profiles or model not in(self.config.profiles[profile].model,*self.config.profiles[profile].backup_models)):
            raise ValueError('Cooldown source is outside selected worker profile')
        with self._locked()as(directory,value):
            if any(x['attempt_id']==attempt for x in value['entries']):return
            now=self.clock();entries=[x for x in value['entries']if x['blocked_until']>now and(x['profile'],x['model'])!=(profile,model)]
            entries.append(dict(profile=profile,model=model,attempt_id=attempt,blocked_until=now+(diagnostic['retry_after_seconds']or 900),diagnostic=diagnostic))
            if len(entries)>MAX_ENTRIES:raise ValueError('Cooldown state exceeds entry bound')
            payload=json.dumps(dict(version=1,entries=entries),sort_keys=True,allow_nan=False,separators=(',',':')).encode()
            if len(payload)>MAX_BYTES:raise ValueError('Cooldown state exceeds bound')
            name='state.'+uuid4().hex+'.new'
            try:
                fd=os.open(name,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=directory)
                with os.fdopen(fd,'wb')as stream:stream.write(payload);stream.flush();os.fsync(stream.fileno())
                os.replace(name,'state.json',src_dir_fd=directory,dst_dir_fd=directory);os.fsync(directory)
            finally:
                try:os.unlink(name,dir_fd=directory)
                except FileNotFoundError:pass
    def selection(self):
        configured={(alias,model)for alias,profile in self.config.profiles.items()for model in(profile.model,*profile.backup_models)}
        with self._locked()as(_directory,value):
            # The state root may be shared by workers with different selected
            # profile/model sets. Validate every retained record, but only its
            # matching execution scope may withhold this worker's admission.
            blocked={(x['profile'],x['model'])for x in value['entries']if x['blocked_until']>self.clock()
                and(x['profile'],x['model'])in configured}
        advertised=[x for x in self.config.advertised_profiles()if(x['alias'],x['model'])not in blocked]
        aliases={x['alias']for x in advertised}
        # Existing unattended AGY SQL claim checks exact advertised model. Older
        # generic policies only check alias: withhold that entire alias there.
        if self.config.resource_profile!=AGY_POLICY_ID:
            aliases-={alias for alias,_model in blocked}
            advertised=[x for x in advertised if x['alias']in aliases]
        return tuple(x for x in self.config.allowed_profiles if x in aliases),advertised


class ProviderCooldownFactory:
    def __init__(self,config,*,clock=time.time):self.config,self.clock=config,clock
    def create(self):return ProviderCooldown(self.config,clock=self.clock)
