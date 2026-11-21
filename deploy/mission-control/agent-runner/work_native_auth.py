"""Attempt-owned native credentials; cleanup needs separately proven process stop."""
import os
import stat

class AuthCleanupError(ValueError):
    def __init__(self,identity):
        super().__init__('Copied native authentication requires reconciliation')
        self.identity=identity


def _open_directory(path):
    """Walk owned absolute state paths without following any parent symlink."""
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('Native state directory must be absolute')
    directory=os.open('/',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:
        for part in path.parts[1:]:
            child=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=directory)
            os.close(directory);directory=child
        result=directory;directory=None
        return result
    finally:
        if directory is not None:os.close(directory)


class AuthLease:
    """Own only the staged inode; never unlink through native-writable paths.

    The caller must prove that its process scope is empty, or never started,
    before cleanup. Retained descriptors survive parent directory replacement.
    """
    def __init__(self,directory,credential,name,relative_directory="home/.grok"):
        self.directory=directory;self.credential=credential;self.name=name
        self.cleaned=False
        parent=os.fstat(directory);inode=os.fstat(credential)
        self.identity={'version':1,'relative_directory':relative_directory,'name':name,
                       'directory_device':parent.st_dev,'directory_inode':parent.st_ino,
                       'credential_device':inode.st_dev,'credential_inode':inode.st_ino}

    def cleanup(self):
        if self.cleaned:return True
        if self.directory is None or self.credential is None:return False
        try:
            inode=os.fstat(self.credential)
            if inode.st_nlink==0:
                self.cleaned=True;return True
            # Never remove an unrelated replacement or claim that extra hard
            # links to the credential have been removed.
            current=os.stat(self.name,dir_fd=self.directory,follow_symlinks=False)
            if (not stat.S_ISREG(current.st_mode) or inode.st_nlink!=1
                    or (current.st_dev,current.st_ino)!=(inode.st_dev,inode.st_ino)):
                return False
            os.unlink(self.name,dir_fd=self.directory)
            self.cleaned=os.fstat(self.credential).st_nlink==0
            return self.cleaned
        except OSError:return False

    def close(self):
        for name in ('credential','directory'):
            descriptor=getattr(self,name,None)
            if descriptor is not None:
                setattr(self,name,None)
                os.close(descriptor)

    def __del__(self):
        self.close()
