//
// Copyright 2024 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

package org.signal.libsignal.usernames;

public final class DiscriminatorCannotHaveLeadingZerosException extends BadDiscriminatorException {
  public DiscriminatorCannotHaveLeadingZerosException(String message) {
    super(message);
  }
}
